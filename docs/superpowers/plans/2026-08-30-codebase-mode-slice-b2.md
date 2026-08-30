# Codebase Mode Slice B2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `capability bench --codebase <PATH>` gains `--allow-exec`, and behind that one gate the two tiers that say whether a fill is *code* rather than merely *plausible text*: tier 6 runs `cargo check` over the spliced worktree and reads the JSON diagnostics, tier 7 runs the repository's own covering tests for the masked symbol.

**Architecture:** One new module, `core/bench/codebase/exec.rs`, owns everything that runs a subprocess: the toolchain probe, the scratch `CARGO_TARGET_DIR`, the timed process-group-killed cargo runner, the splice, the diagnostics parser, the revert-and-verify, the enclosing-function and crate lookups, covering-test discovery and the test run. `prepare` keeps the `Worktree` alive when exec is on (as `Prepared.exec: Exec`, a three-state enum whose `Ready` variant owns the worktree, the target dir and the `cargo --version` line); `run::run_codebase` hooks one exec step per crossing between the infill and the row. `store.rs` gains an owned, serde-able `ExecScore` and an `ExecRow`, two cells per tier line, two lift columns and a timing/skip trailer. `stamp.rs` gains three environment fields. Everything below the live run is unit-tested with a fake `cargo` (a shell script on `$CHEKOV_CARGO`); the one real-toolchain test is gated on `CHEKOV_TEST_EXEC=1`.

**Tech Stack:** Rust (edition 2024, toolchain pinned 1.95.0), `serde`/`serde_json` (the diagnostics stream), `toml` (the crate manifest), `nix` with the `signal` feature (the process-group kill), `std::process::Command`; **no new crate**.

**Spec:** `docs/superpowers/specs/2026-08-30-codebase-mode-slice-b2-design.md` (builds on `2026-08-29-codebase-mode-slice-b1-design.md` and `2026-08-29-codebase-mode-slice-a-design.md`; umbrella `docs/capability-spec.md` §8).

## Global Constraints

- Rust 2024; **no new crates**. `serde_json` is already a dependency (the diagnostics stream); `toml` is already a dependency (the crate manifest); `std::process::Command` spawns cargo.
- **Timeouts are hand-rolled** — `std::thread::sleep` + `Child::try_wait` polling at 50 ms. No `wait-timeout` crate.
- **Process-group kill uses `nix`, which IS already a dependency** (`nix = { version = "0.31", features = ["signal"] }` in `Cargo.toml:54`; `src/core/server.rs:69,74` already calls `nix::sys::signal::kill` with `nix::unistd::Pid::from_raw`). `libc` is **not** a direct dependency and must not become one; do **not** shell out to `kill(1)`. The child is put in its own process group with `std::os::unix::process::CommandExt::process_group(0)` (stable since 1.64), so its pgid **is** its pid, and the kill is `nix::sys::signal::kill(Pid::from_raw(-pid), Signal::SIGKILL)`.
- Every function ≤ 40 LOC, ≤ 3 parameters (bundle into a struct past that), nesting ≤ 3 — `clippy.toml` sets `too-many-arguments-threshold = 3`, `too-many-lines-threshold = 40`, `excessive-nesting-threshold = 4`; `cargo clippy --locked --all-targets -- -D warnings` with the crate's pedantic+nursery set is the gate; `#[allow]`/`#[expect]` are blocked by pushkin — extract a helper instead.
- clippy's `float_cmp` is on — f64 test assertions go through an `approx` helper; `missing_const_for_fn`, `too_long_first_doc_paragraph`, `similar_names`, `default_trait_access`, `cast_precision_loss` (use `ladder::as_f64`) and `case_sensitive_file_extension_comparisons` all fire in this crate.
- Every `ChekovError` Display names its remediation; nothing degrades silently — a skip is a counted reason, never a zero and never a pass.
- `CodebaseRow` keeps `#[serde(deny_unknown_fields)]`; every new field is `#[serde(default)]` so slice-A/B1 rows load. `Stamp` keeps `deny_unknown_fields`; its three new fields are `#[serde(default)]` so older `stamp.json` files load.
- `--allow-exec` is the single gate: without it no `cargo` is ever spawned; with it, every cargo invocation **after the one `cargo fetch`** carries `--offline`, and every one of them (the fetch included) carries `CARGO_TARGET_DIR` pointing at the scratch dir. Wall-clock timeouts are 120 s (check) / 300 s (test) with process-group kill. The worktree is the only place written. Revert-and-verify runs after **every** crossing; `ExecWorktreeDirty` aborts the run.
- **Ruled during execution (Task 2):** `src/lib.rs` carries `#![forbid(unsafe_code)]`, so the `unsafe { set_var("CHEKOV_CARGO") }` test seam this plan originally specified cannot exist in the crate. The seam is explicit instead: `CargoRun.program: &Path` and `Env.cargo: PathBuf`, resolved once in `probe` from `$CHEKOV_CARGO` (a read, which is safe) or `"cargo"`; tests hand the fake script's path to `CargoRun`/`Env` directly. No environment write, no mutex, no `unsafe` anywhere. Tasks 3–5 construct `Env { cargo: <fake>, … }` in their tests accordingly.
- Commit trailer on every commit (both lines verbatim):
  `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`
  `Claude-Session: https://claude.ai/code/session_01YDaTTHJySW2ix3P85BcP6W`
- `pushkin floor` (fmt + clippy + tests) green before every commit.
- Bash chains use `&&`, never `;`; never `cd`; src files are read with ranged `Read` (`offset`/`limit`) and written with `Edit` — whole-file reads and `cat`/`grep`/`sed` on `src/**` are blocked by the pushkin gate.
- **Line numbers in this plan are from HEAD `a33206e`.** Locate every edit site by the symbol named beside the number (`render_codebase`, `assemble`, `prepare`, `run_codebase`, …), never by the number alone; if the two disagree, the symbol wins.

---

## Decisions this plan takes where the spec left a choice

Four places where the spec names an outcome without naming the mechanism. Each is settled here once, and every task below assumes the settlement.

**1. `Score` reuse → a new `ExecScore`, not a `Cow` on `ladder::Score`.**
`ladder::Score` is `#[derive(Debug, Clone, Copy, PartialEq)] { Value(f64), Skipped(&'static str) }` (`ladder.rs:41-45`). The exec reasons carry cargo's own words (`needs network: <message>`), which are owned, and `ExecRow` is **serialised**, which `Score` is not (it derives no serde). Widening `Score` to `Cow<'static, str>` would cost it `Copy` and ripple through `stored_tier`, `score_all`, `recompute`, `tier_mean` and every ladder test for the four tiers that never needed it. So `store.rs` gets its own:
```rust
pub enum ExecScore { Value(f64), Skipped(String) }
```
`ladder::Score` is untouched, and `ladder::score_all` keeps returning `Compile`/`Test` as `Skipped(EXEC_SKIPPED)` — the exec tiers are scored by the run loop, never by the ladder (spec §5).

**2. The task's `byte_range` is in the ORIGINAL file's coordinates, and `filter` computes it.**
Spec §3 step 1 says "read the worktree's *original* F (test modules intact) … replace the bytes of the span (`byte_range` on the task)". `CodebaseTask` has no `byte_range` today, and `masker::Candidate.byte_range` indexes the **elided** text — the text with `#[cfg(test)]` items cut out. Splicing into the elided text is not an option: tier 7 has to run those very test modules. Since `filter::elide_cfg_test` only ever **deletes** whole ranges (`filter.rs:45-66`), the map back is exact — add the removed ranges to `Elided`, and shift an elided offset by the length of every cut that precedes it. `CodebaseTask` gains `byte_range: Range<usize>` in original coordinates, and the invariant every test asserts is `&original[task.byte_range] == task.gold`.

**3. Worktree lifetime → `Prepared.exec: Exec`, a three-state enum owning the worktree.**
```rust
pub enum Exec { Off, Unavailable(String), Ready(Env) }
pub struct Env { worktree: tree::Worktree, target_dir: PathBuf, cargo_version: String, timeouts: Timeouts }
```
`Off` and `Unavailable` both remove the worktree inside `prepare`, exactly as today. `Ready` keeps it; `Worktree`'s existing `Drop` still removes it on every early exit, and `bench` calls an explicit `Env::finish()` after the candidate loop so a cleanup failure is *reported* rather than swallowed by `Drop`. It lives on `Prepared` and not on a second handle in `RunInputs` because `Prepared` is already the value that outlives every candidate and is already threaded to `run_codebase`.

**4. Report spacing follows the code, not the spec's sketch.**
Spec §6 draws the cells with two spaces; `store::scores_line` joins with `cells.join("   ")` (three) and `lift_line` with `cells.join("  ")` (two). The exec cells are pushed onto those same `cells` vectors, so they inherit the existing join and the columns stay aligned. Every exact-string test in this plan is written against the code's spacing.

---

## File structure

| File | Responsibility in this slice |
|---|---|
| `src/core/bench/codebase/exec.rs` | **new** — `Timeouts`, `CargoRun`/`CargoOutcome`/`run_cargo` (timeout + process-group kill), `probe`, `prepare_env`, `Exec`/`Env`, `Splice`/`spliced`/`apply`, `first_error`, `needs_network`, `revert`, `Crate`/`crate_of`, `covering_tests`, `run_tests`, `exec_crossing` |
| `src/core/bench/codebase/masker.rs` | `enclosing_fn(text, at) -> Option<String>` over the existing private `fn_signatures`/`body_after` |
| `src/core/bench/codebase/filter.rs` | `Elided.cuts`; `original_range`; `Context.cuts`; `assemble` fills `CodebaseTask.byte_range` |
| `src/core/bench/codebase/ladder.rs` | `trimmed_to_gold` becomes `pub(super)` — the splice grades the same text tiers 1–4 do |
| `src/core/bench/codebase/tree.rs` | `git` becomes `pub(super)` — the revert is `git checkout --` in the worktree |
| `src/core/bench/codebase/mod.rs` | `CodebaseTask.byte_range`; `Prepared.exec`; `PrepareInputs`; `FileElision`; `prepare` keeps or removes the worktree by the gate |
| `src/core/bench/codebase/run.rs` | one exec step per crossing, the skip-by-reason paths, the live estimate line, `Recorded.exec` |
| `src/core/bench/store.rs` | `ExecScore`, `ExecRow`, `CodebaseRow.exec`, the two cells, the two lift columns, the trailer, the header clause |
| `src/core/bench/stamp.rs` | `allow_exec`, `cargo_version`, `exec_target`; the array becomes 20 |
| `src/error.rs` | `ExecWorktreeDirty { path, file }` + its remediation test |
| `src/commands/capability.rs` | `--allow-exec`, `BenchArgs.allow_exec`, `PrepareInputs`, the estimate, the dry-run clause, `CodebaseHead`, `Env::finish` after the loop |
| `tests/codebase_exec.rs` | **new** — the real-cargo integration test behind `CHEKOV_TEST_EXEC=1` |
| `README.md`, `CHANGELOG.md`, `IDEAS.md`, `docs/capability-spec.md` | docs |

---

### Task 1: The gate, the stamp's three fields, the error, and the row's `exec`

Everything the later tasks write into, with nothing yet writing into it. A reviewer can reject this task on the schema alone.

**Files:**
- Modify: `src/commands/capability.rs:98-127` (`BenchOpts`), `:777-800` (`BenchArgs` + `From`)
- Modify: `src/core/bench/stamp.rs:15-41` (`Stamp`), `:45-75` (`first_mismatch`), `:122-142` (the test fixture)
- Modify: `src/error.rs:346-350` (beside `CodebaseWorktreeFailed`), `:374-399` (`codebase_errors_name_their_remediation`)
- Modify: `src/core/bench/store.rs:132-178` (`ExecScore`, `ExecRow`, `CodebaseRow.exec`), test fixture `codebase_task` at `:1567-1598`
- Modify: `src/core/bench/codebase/run.rs:410-436` (`run_head` test fixture)
- Modify: `src/core/bench/compare.rs` (`stamp` test fixture, ~`:798`), `src/core/bench/speeds.rs` (~`:186`), `src/core/bench/store.rs` (`stamp` test fixture, ~`:1157`)
- Modify: `src/commands/capability.rs:1573-1594` (`build_head`'s `Stamp` literal)

**Interfaces:**
- Produces:
  ```rust
  // commands/capability.rs
  pub struct BenchOpts { /* … */ pub allow_exec: bool }   // #[arg(long)]
  struct BenchArgs<'a> { /* … */ allow_exec: bool }

  // core/bench/stamp.rs — inserted after `flash_attn`, before `seed`
  pub struct Stamp {
      /* … flash_attn … */
      #[serde(default)] pub allow_exec: bool,
      #[serde(default)] pub cargo_version: Option<String>,
      #[serde(default = "exec_target_off")] pub exec_target: String,
      /* … seed … */
  }
  fn exec_target_off() -> String;                 // "none"
  pub const EXEC_TARGET_SCRATCH: &str = "scratch";
  pub const EXEC_TARGET_OFF: &str = "none";

  // core/bench/store.rs
  #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
  #[serde(tag = "kind", content = "value", rename_all = "snake_case")]
  pub enum ExecScore { Value(f64), Skipped(String) }

  #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
  #[serde(deny_unknown_fields)]
  pub struct ExecRow {
      pub compile: ExecScore,
      pub compile_error: Option<String>,   // "<file>:<line>: <message>"
      pub tests: Vec<String>,              // candidates run, in order, <= 5
      pub test: ExecScore,
      pub test_failure: Option<String>,    // "<test>: <cargo's text>"
      pub check_secs: f64,
      pub test_secs: f64,
  }
  impl ExecRow { pub fn skipped(reason: &str) -> Self; }
  pub struct CodebaseRow { /* … */ pub exec: Option<ExecRow> }

  // error.rs
  ChekovError::ExecWorktreeDirty { path: PathBuf, file: String }
  ```
- Consumes: nothing from later tasks.

**Why `exec_target` is a `String` and not a bool:** the spec pins it to the literal `"scratch"`, and the field exists so a later slice that runs against the repository's own `target/` can be told apart from this one by `compare`'s first-differing-field rule. A bool would have to be renamed to say that.

- [ ] **Step 1: Write the failing test for the error's remediation**

In `src/error.rs`, inside `mod tests`, extend `codebase_errors_name_their_remediation` (`:375`) — add this to the end of its body, before the closing brace:

```rust
        let dirty = ChekovError::ExecWorktreeDirty {
            path: "/eval/.scratch/codebase-tree-abc123def456".into(),
            file: "src/core/bench/store.rs".into(),
        }
        .to_string();
        assert!(
            dirty.contains("src/core/bench/store.rs"),
            "the file that would not restore is named: {dirty}"
        );
        assert!(
            dirty.contains("/eval/.scratch/codebase-tree-abc123def456"),
            "the worktree to inspect is named: {dirty}"
        );
        assert!(
            dirty.contains("rm -rf") || dirty.contains("delete"),
            "the remediation says to delete it: {dirty}"
        );
        assert!(
            dirty.contains("--resume"),
            "and that the rows already written are resumable: {dirty}"
        );
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test --locked --lib error::tests::codebase_errors_name_their_remediation`
Expected: FAIL — `no variant named ExecWorktreeDirty found for enum ChekovError`.

- [ ] **Step 3: Add the variant**

In `src/error.rs`, immediately after `CodebaseWorktreeFailed` (`:350`) and before the `Io` variant:

```rust
    #[error(
        "the codebase worktree at {} could not be restored: {file} still differs from HEAD \
         after `git checkout` — the run stopped rather than measure the next crossing against \
         a file it cannot vouch for; inspect the worktree and then delete it (`git worktree \
         remove --force {}` in the repository, or `rm -rf {}` plus `git worktree prune`); the \
         rows already written are intact, so `--resume <RUN>` picks the run up at this crossing",
        path.display(),
        path.display(),
        path.display()
    )]
    ExecWorktreeDirty { path: PathBuf, file: String },
```

- [ ] **Step 4: Run it and watch it pass**

Run: `cargo test --locked --lib error::tests::codebase_errors_name_their_remediation`
Expected: PASS (1 passed).

- [ ] **Step 5: Write the failing test for the stamp's three fields**

In `src/core/bench/stamp.rs`, inside `mod tests`, add:

```rust
    #[test]
    fn the_exec_fields_refuse_like_any_other_environment_field() {
        let mut b = stamp();
        b.allow_exec = true;
        assert_eq!(first_mismatch(&stamp(), &b), Some("allow_exec"));
        let mut b = stamp();
        b.cargo_version = Some("cargo 1.95.0 (0000000 2026-01-01)".into());
        assert_eq!(first_mismatch(&stamp(), &b), Some("cargo_version"));
        let mut b = stamp();
        b.exec_target = "scratch".into();
        assert_eq!(first_mismatch(&stamp(), &b), Some("exec_target"));
        // Declaration order: an environment field still loses to the identity
        // fields above it, and still beats the seed below it.
        let mut b = stamp();
        b.allow_exec = true;
        b.machine_id = "0000".into();
        assert_eq!(first_mismatch(&stamp(), &b), Some("machine_id"));
        let mut b = stamp();
        b.allow_exec = true;
        b.seed = 43;
        assert_eq!(first_mismatch(&stamp(), &b), Some("allow_exec"));
    }

    /// A stamp written before B2 has none of the three. It must still load —
    /// and load as what it was: a run that never ran anything.
    #[test]
    fn a_pre_b2_stamp_loads_with_exec_off() {
        let json = r#"{"machine_id":"m","engine_build_commit":"e","weights_revision":"w",
            "quant":"Q8_0","ctx":4096,"n_parallel":1,"kv_unified":"engine-default",
            "n_batch":"engine-default","n_ubatch":"engine-default","type_k":"q8_0",
            "type_v":"q8_0","flash_attn":"on","seed":42,"temperature_milli":0,
            "chekov_version":"0.1.0","prompt_set_hash":"e19a","corpus_id":"throughput-v1"}"#;
        let parsed: Stamp = serde_json::from_str(json).expect("a pre-B2 stamp loads");
        assert!(!parsed.allow_exec);
        assert_eq!(parsed.cargo_version, None);
        assert_eq!(parsed.exec_target, "none");
    }
```

- [ ] **Step 6: Run them and watch them fail**

Run: `cargo test --locked --lib bench::stamp::tests`
Expected: FAIL — `no field allow_exec on type Stamp`.

- [ ] **Step 7: Add the three fields**

In `src/core/bench/stamp.rs`, between `flash_attn` (`:34`) and `seed` (`:35`):

```rust
    /// Whether `--allow-exec` was given. Runs that executed the repository and
    /// runs that only read it are not the same environment: tiers 6-7 exist in
    /// one and are absent from the other, so `compare` refuses across it.
    #[serde(default)]
    pub allow_exec: bool,
    /// The `cargo --version` line, when exec ran. `None` both when the flag was
    /// absent and when the machine had no toolchain — the report tells those
    /// two apart from the rows, not from here.
    #[serde(default)]
    pub cargo_version: Option<String>,
    /// Where the build artefacts went: `"scratch"` for the run's own
    /// `CARGO_TARGET_DIR`, `"none"` when nothing was built. A later slice that
    /// reuses the repository's `target/` is a different environment and this
    /// field is how `compare` will say so.
    #[serde(default = "exec_target_off")]
    pub exec_target: String,
```

Below the struct, before `first_mismatch`:

```rust
/// A stamp written before the exec tiers existed ran nothing, and says so.
fn exec_target_off() -> String {
    EXEC_TARGET_OFF.to_owned()
}

/// `exec_target` when the run built into its own scratch directory.
pub const EXEC_TARGET_SCRATCH: &str = "scratch";
/// `exec_target` when the run built nothing at all.
pub const EXEC_TARGET_OFF: &str = "none";
```

In `first_mismatch` (`:46`), widen the array to 20 and insert the three rows between `flash_attn` and `seed`:

```rust
    let pairs: [(&'static str, bool); 20] = [
        // … machine_id … flash_attn unchanged …
        ("allow_exec", a.allow_exec != b.allow_exec),
        ("cargo_version", a.cargo_version != b.cargo_version),
        ("exec_target", a.exec_target != b.exec_target),
        ("seed", a.seed != b.seed),
        // … temperature_milli … corpus_id unchanged …
    ];
```

Also update the module doc (`stamp.rs:1`): `//! The 20-field configuration stamp (spec §7.4).`

- [ ] **Step 8: Fix every `Stamp { … }` literal the compiler names**

Run: `cargo check --locked --all-targets`
Expected: FAIL, once per literal, with `missing fields allow_exec, cargo_version and exec_target`. Add these three lines to each, immediately after `flash_attn`:

```rust
            allow_exec: false,
            cargo_version: None,
            exec_target: "none".into(),
```

The sites are `src/core/bench/stamp.rs` (`stamp()`, ~`:123`), `src/core/bench/store.rs` (`stamp()`, ~`:1157`), `src/core/bench/compare.rs` (~`:798`), `src/core/bench/speeds.rs` (~`:186`), `src/core/bench/codebase/run.rs` (`run_head()`, ~`:416`), and `src/commands/capability.rs` (`build_head`, `:1573`). In `build_head` the three lines are **not** literals — they come from the run, so write them as:

```rust
        allow_exec: inputs.allow_exec(),
        cargo_version: inputs.cargo_version().map(str::to_owned),
        exec_target: if inputs.allow_exec() {
            stamp::EXEC_TARGET_SCRATCH.to_owned()
        } else {
            stamp::EXEC_TARGET_OFF.to_owned()
        },
```

and add the two accessors to `HeadInputs` (`capability.rs:1512`), which in this task still read from nothing and so answer `false`/`None` — Task 7 wires them to the prepared run:

```rust
impl HeadInputs<'_> {
    /// Whether this run was allowed to execute the repository. Task 7 gives
    /// `codebase` an exec half; until then no run executes anything.
    const fn allow_exec(&self) -> bool {
        false
    }

    /// The `cargo --version` line, when exec ran.
    const fn cargo_version(&self) -> Option<&str> {
        None
    }
}
```

- [ ] **Step 9: Run the stamp tests and watch them pass**

Run: `cargo test --locked --lib bench::stamp::tests`
Expected: PASS (4 passed) — the two new ones plus the two that were already there.

- [ ] **Step 10: Write the failing test for `ExecRow` on the row**

In `src/core/bench/store.rs`, inside `mod tests`, add:

```rust
    /// The row a run with `--allow-exec` writes, and the one a run without it
    /// writes, both round-trip — and a pre-B2 row loads as the second.
    #[test]
    fn an_exec_row_round_trips_and_a_pre_b2_row_loads_without_one() {
        let mut task = codebase_task(CodebaseFixture {
            id: "in_file-abc123-L7",
            tier: TaskTier::InFile,
            gold: "let a = 1;",
            prediction: "let a = 1;",
        });
        if let Some(row) = task.codebase.as_mut() {
            row.exec = Some(super::ExecRow {
                compile: super::ExecScore::Value(1.0),
                compile_error: None,
                tests: vec!["covers_alpha".into()],
                test: super::ExecScore::Skipped("did not compile".into()),
                test_failure: None,
                check_secs: 6.25,
                test_secs: 0.0,
            });
        }
        let row = task.codebase.expect("a codebase row");
        let text = serde_json::to_string(&row).expect("serialise");
        let back: super::CodebaseRow = serde_json::from_str(&text).expect("deserialise");
        let exec = back.exec.expect("the exec half survives the round trip");
        assert_eq!(exec.compile, super::ExecScore::Value(1.0));
        assert_eq!(
            exec.test,
            super::ExecScore::Skipped("did not compile".into())
        );
        assert_eq!(exec.tests, vec!["covers_alpha".to_owned()]);

        let pre_b2 = r#"{"tier":"in_file","file":"src/a.rs","line":7,
            "label":"boundary-scanned (not AST)","gold":"let a = 1;","prediction":"let a = 1;",
            "prefix":"fn f() {\n","suffix":"\n}\n",
            "excluded":{"doc_comment":0,"cross_file":"n/a: same-file"}}"#;
        let old: super::CodebaseRow = serde_json::from_str(pre_b2).expect("a pre-B2 row loads");
        assert!(old.exec.is_none(), "a run that never executed has no exec half");
    }

    /// A skip is a reason, never a zero: `ExecRow::skipped` measures nothing
    /// and scores nothing.
    #[test]
    fn a_wholly_skipped_exec_row_carries_the_reason_on_both_tiers() {
        let row = super::ExecRow::skipped("no Rust toolchain: cargo not on PATH");
        assert_eq!(
            row.compile,
            super::ExecScore::Skipped("no Rust toolchain: cargo not on PATH".into())
        );
        assert_eq!(row.compile, row.test, "one reason covers both tiers");
        assert!(row.tests.is_empty());
        assert!(row.compile_error.is_none() && row.test_failure.is_none());
    }
```

- [ ] **Step 11: Run them and watch them fail**

Run: `cargo test --locked --lib bench::store::tests::an_exec_row_round_trips_and_a_pre_b2_row_loads_without_one`
Expected: FAIL — `cannot find type ExecRow in module super`.

- [ ] **Step 12: Add `ExecScore`, `ExecRow` and the field**

In `src/core/bench/store.rs`, immediately above `CodebaseRow` (`:132`):

```rust
/// Tier 6 or tier 7's outcome for one crossing.
///
/// Not `ladder::Score`: that one is `Copy` over a `&'static str`, and these
/// reasons carry cargo's own words. It is also serialised, and `Score` is not.
/// Widening `Score` would cost the four text tiers their `Copy` for a reason
/// none of them has.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ExecScore {
    Value(f64),
    Skipped(String),
}

/// Tiers 6-7 for one crossing, measured at run time and never recomputed.
///
/// A compile result cannot be re-derived from stored text the way tiers 1-4
/// can — the toolchain, the worktree and the rest of the workspace all went
/// into it — so this is the one part of a codebase row that is a stored score.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecRow {
    pub compile: ExecScore,
    /// `<file>:<line>: <message>` from the first `error` diagnostic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compile_error: Option<String>,
    /// The covering tests actually run, in file order, at most five.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tests: Vec<String>,
    pub test: ExecScore,
    /// `<test>: <cargo's text>` for the first candidate that failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_failure: Option<String>,
    pub check_secs: f64,
    pub test_secs: f64,
}

impl ExecRow {
    /// Both tiers skipped for one reason, nothing measured — what a crossing
    /// records when the machine could not have run either of them.
    #[must_use]
    pub fn skipped(reason: &str) -> Self {
        Self {
            compile: ExecScore::Skipped(reason.to_owned()),
            compile_error: None,
            tests: Vec::new(),
            test: ExecScore::Skipped(reason.to_owned()),
            test_failure: None,
            check_secs: 0.0,
            test_secs: 0.0,
        }
    }
}
```

On `CodebaseRow`, after `n_predict` (`:177`):

```rust
    /// Tiers 6-7, when `--allow-exec` was given. `None` when it was not, and
    /// on every row written before B2 — which is what those runs were: runs
    /// that executed nothing, not runs that failed to compile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec: Option<ExecRow>,
```

- [ ] **Step 13: Add `exec: None` to the test fixture and fix the compiler's list**

Run: `cargo check --locked --all-targets`
Expected: FAIL at `codebase_task` (`store.rs:1574`) and at `record_codebase_task` (`run.rs:281`) with `missing field exec`. Add `exec: None,` after `n_predict` at both sites.

- [ ] **Step 14: Run the store tests and watch them pass**

Run: `cargo test --locked --lib bench::store::tests`
Expected: PASS — every existing test plus the two new ones.

- [ ] **Step 15: Add the flag and thread it into `BenchArgs`**

In `src/commands/capability.rs`, on `BenchOpts` after `codebase` (`:126`):

```rust
    /// Run the repository's own build for tiers 6-7 (compile gate, covering
    /// test). The SINGLE gate on every path that executes repository code:
    /// `cargo check` and `cargo test` run its `build.rs`, its proc-macros and
    /// its tests — the same trust as building it yourself. Bounded to a
    /// detached worktree, offline after one fetch, a scratch target directory
    /// and wall-clock timeouts; not a sandbox.
    #[arg(long)]
    pub allow_exec: bool,
```

On `BenchArgs` (`:778`) add `allow_exec: bool,`, and in the `From` impl (`:789`) add `allow_exec: opts.allow_exec,`.

- [ ] **Step 16: Write the failing test that the flag is parsed and defaults off**

In `src/commands/capability.rs`'s `mod tests`, add:

```rust
    #[test]
    fn allow_exec_defaults_off_and_is_a_bare_switch() {
        use clap::Parser;

        #[derive(clap::Parser)]
        struct Wrap {
            #[command(flatten)]
            opts: super::BenchOpts,
        }

        let off = Wrap::parse_from(["bench", "--codebase", "."]);
        assert!(!off.opts.allow_exec, "nothing executes unless it is asked for");
        let on = Wrap::parse_from(["bench", "--codebase", ".", "--allow-exec"]);
        assert!(on.opts.allow_exec);
    }
```

- [ ] **Step 17: Run it and watch it pass**

Run: `cargo test --locked --lib commands::capability::tests::allow_exec_defaults_off_and_is_a_bare_switch`
Expected: PASS.

- [ ] **Step 18: Run the floor and commit**

Run: `cargo fmt && cargo clippy --locked --all-targets -- -D warnings && cargo test --locked`
Expected: clean; all tests pass.

```bash
git add src/error.rs src/core/bench/stamp.rs src/core/bench/store.rs src/core/bench/compare.rs src/core/bench/speeds.rs src/core/bench/codebase/run.rs src/commands/capability.rs && git commit -m "$(cat <<'EOF'
feat(bench): the --allow-exec gate, the stamp's three exec fields, and the row that will hold tiers 6-7

ExecScore is its own owned, serde-able score rather than a Cow on
ladder::Score: the exec reasons carry cargo's words, the row is stored,
and the four text tiers keep their Copy.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01YDaTTHJySW2ix3P85BcP6W
EOF
)"
```

---

### Task 2: `codebase/exec.rs` part 1 — the toolchain probe, the scratch target, and the timed cargo runner

The subprocess machinery, standing alone. No unit test in this task spawns a real `cargo`: every one of them points `$CHEKOV_CARGO` at a shell script the test writes.

**Files:**
- Create: `src/core/bench/codebase/exec.rs`
- Modify: `src/core/bench/codebase/mod.rs:4-10` (declare `pub mod exec;`)
- Modify: `src/core/bench/codebase/tree.rs:16` (`git` becomes `pub(super)`)

**Interfaces:**
- Consumes: `tree::Worktree { pub path: PathBuf }`, `tree::Worktree::remove(self) -> Result<(), ChekovError>` (`tree.rs:61,98`); `ChekovError::io(context, source)` (`error.rs:362`).
- Produces:
  ```rust
  // codebase/exec.rs
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub struct Timeouts { pub check: Duration, pub test: Duration }
  impl Timeouts { pub const DEFAULT: Self; }        // 120 s / 300 s

  pub struct CargoRun<'a> {
      pub args: &'a [&'a str],
      pub cwd: &'a Path,
      pub target_dir: &'a Path,
      pub timeout: Duration,
  }
  pub struct CargoOutcome {
      pub status: Option<i32>,
      pub stdout: String,
      pub stderr: String,
      pub secs: f64,
      pub timed_out: bool,
  }
  pub fn run_cargo(run: &CargoRun) -> Result<CargoOutcome, ChekovError>;

  pub struct Env {
      pub worktree: super::tree::Worktree,
      pub target_dir: PathBuf,
      pub cargo_version: String,
      pub timeouts: Timeouts,
  }
  impl Env { pub fn finish(self) -> Result<(), ChekovError>; }

  pub enum Exec { Off, Unavailable(String), Ready(Env) }
  impl Exec {
      pub const fn allowed(&self) -> bool;
      pub fn cargo_version(&self) -> Option<&str>;
      pub fn env(&self) -> Option<&Env>;
      pub fn finish(self) -> Result<(), ChekovError>;
  }

  pub fn probe(root: &Path) -> Result<String, String>;
  pub fn prepare_env(worktree: super::tree::Worktree, scratch_root: &Path, head12: &str)
      -> Result<Exec, ChekovError>;

  // codebase/tree.rs
  pub(super) fn git(repo: &Path, args: &[&str], step: &str) -> Result<String, ChekovError>;
  ```

**Why the pipes are drained on threads:** `cargo check --message-format=json` writes megabytes to stdout. A `try_wait` loop that never reads the pipe wedges the very process it is timing as soon as the 64 KiB buffer fills, and the "timeout" it then reports would be chekov's own deadlock. Two `std::thread::spawn`ed readers own the two pipes; the main thread polls the clock.

- [ ] **Step 1: Widen `tree::git` so the revert can use it**

In `src/core/bench/codebase/tree.rs:16`, change `fn git(` to `pub(super) fn git(`, and put the reason above it:

```rust
/// `git -C repo <args>`, with the step named in the failure.
///
/// `pub(super)` for `exec::revert`: undoing a splice is `git checkout --` in
/// the same worktree, and a second spawn helper beside this one would be a
/// second place for the error contract to drift.
pub(super) fn git(repo: &Path, args: &[&str], step: &str) -> Result<String, ChekovError> {
```

- [ ] **Step 2: Declare the module**

In `src/core/bench/codebase/mod.rs`, add `pub mod exec;` to the module list (`:4-10`), keeping it alphabetical — between `pub mod crossfile;` and `pub mod filter;`.

- [ ] **Step 3: Write the failing tests**

Create `src/core/bench/codebase/exec.rs` with **only** the module doc and this test module (the production code arrives in Step 5):

```rust
//! Tiers 6 and 7: what happens when the fill is actually built.
//!
//! Everything in this module runs a subprocess, and nothing in it runs
//! without `--allow-exec`. The bounds are the worktree (the only place
//! written), `--offline` after one fetch, a scratch `CARGO_TARGET_DIR`, a
//! wall-clock timeout with a process-group kill, and a revert verified byte
//! for byte before the next crossing is measured.

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use super::{CargoRun, Timeouts};

    /// A scratch directory keyed by name, cleared first: two tests that both
    /// want a fake cargo must not share one.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("chekov-test-exec").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    /// An executable shell script standing in for `cargo`, pointed at by
    /// `$CHEKOV_CARGO`. No unit test in this module needs a toolchain.
    fn fake_cargo(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("fake-cargo");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write fake cargo");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod fake cargo");
        path
    }

    /// `$CHEKOV_CARGO` is process-wide, so these tests must not interleave.
    /// One mutex, held across each test that sets it.
    static CARGO_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn a_cargo_run_reports_its_streams_its_status_and_its_wall_clock() {
        let guard = CARGO_ENV.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = scratch("streams");
        let cargo = fake_cargo(&dir, "echo out-line\necho err-line >&2\nexit 3");
        // SAFETY-equivalent note: the mutex above serialises every writer.
        unsafe { std::env::set_var("CHEKOV_CARGO", &cargo) };
        let outcome = super::run_cargo(&CargoRun {
            args: &["check"],
            cwd: &dir,
            target_dir: &dir.join("target"),
            timeout: Duration::from_secs(30),
        })
        .expect("the fake cargo runs");
        unsafe { std::env::remove_var("CHEKOV_CARGO") };
        drop(guard);
        assert_eq!(outcome.status, Some(3));
        assert!(outcome.stdout.contains("out-line"), "{outcome:?}", outcome = outcome.stdout);
        assert!(outcome.stderr.contains("err-line"), "{}", outcome.stderr);
        assert!(!outcome.timed_out);
        assert!(outcome.secs >= 0.0, "the wall clock is recorded");
    }

    /// The timeout is the point: a build script that sleeps forever must not
    /// hold the run, and the whole process GROUP has to go, or `cargo`'s
    /// rustc children outlive it.
    #[test]
    fn a_run_past_its_timeout_is_killed_and_says_so() {
        let guard = CARGO_ENV.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = scratch("timeout");
        // `sh -c 'sleep 30 &  wait'` puts the sleep in a CHILD of the script:
        // killing only the script would leave the sleep behind, so this is the
        // shape that proves the group kill.
        let cargo = fake_cargo(&dir, "sleep 30 &\nwait");
        unsafe { std::env::set_var("CHEKOV_CARGO", &cargo) };
        let started = std::time::Instant::now();
        let outcome = super::run_cargo(&CargoRun {
            args: &["check"],
            cwd: &dir,
            target_dir: &dir.join("target"),
            timeout: Duration::from_millis(300),
        })
        .expect("the runner returns rather than hanging");
        unsafe { std::env::remove_var("CHEKOV_CARGO") };
        drop(guard);
        assert!(outcome.timed_out, "the expiry is reported, not inferred");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the kill happened at the deadline, not at the sleep's end"
        );
    }

    /// A file large enough to fill a pipe buffer: the reader threads are what
    /// keep this from deadlocking, so the test is the reason they exist.
    #[test]
    fn a_chatty_run_does_not_wedge_on_a_full_pipe() {
        let guard = CARGO_ENV.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = scratch("chatty");
        let cargo = fake_cargo(&dir, "i=0\nwhile [ $i -lt 20000 ]; do\n  echo \
                                      'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'\n  \
                                      i=$((i+1))\ndone");
        unsafe { std::env::set_var("CHEKOV_CARGO", &cargo) };
        let outcome = super::run_cargo(&CargoRun {
            args: &["check"],
            cwd: &dir,
            target_dir: &dir.join("target"),
            timeout: Duration::from_secs(60),
        })
        .expect("the fake cargo runs");
        unsafe { std::env::remove_var("CHEKOV_CARGO") };
        drop(guard);
        assert!(!outcome.timed_out, "a chatty child is not a slow one");
        assert!(outcome.stdout.len() > 800_000, "{}", outcome.stdout.len());
    }

    #[test]
    fn the_probe_refuses_a_root_without_a_cargo_toml_and_names_which() {
        let dir = scratch("no-manifest");
        let reason = super::probe(&dir).expect_err("no Cargo.toml, no toolchain");
        assert!(reason.starts_with("no Rust toolchain: "), "{reason}");
        assert!(reason.contains("Cargo.toml"), "{reason}");
    }

    #[test]
    fn the_probe_reports_cargos_version_line_verbatim() {
        let guard = CARGO_ENV.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = scratch("version");
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"x\"\n").expect("manifest");
        let cargo = fake_cargo(&dir, "echo 'cargo 1.95.0 (deadbeef 2026-01-01)'");
        unsafe { std::env::set_var("CHEKOV_CARGO", &cargo) };
        let version = super::probe(&dir).expect("the fake cargo answers --version");
        unsafe { std::env::remove_var("CHEKOV_CARGO") };
        drop(guard);
        assert_eq!(version, "cargo 1.95.0 (deadbeef 2026-01-01)");
    }

    #[test]
    fn a_cargo_that_cannot_run_is_a_missing_toolchain_and_not_an_error() {
        let guard = CARGO_ENV.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = scratch("no-cargo");
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"x\"\n").expect("manifest");
        unsafe { std::env::set_var("CHEKOV_CARGO", dir.join("nothing-here")) };
        let reason = super::probe(&dir).expect_err("nothing to run");
        unsafe { std::env::remove_var("CHEKOV_CARGO") };
        drop(guard);
        assert!(reason.starts_with("no Rust toolchain: "), "{reason}");
    }

    #[test]
    fn the_default_timeouts_are_the_specs_two_minutes_and_five() {
        assert_eq!(Timeouts::DEFAULT.check, Duration::from_secs(120));
        assert_eq!(Timeouts::DEFAULT.test, Duration::from_secs(300));
    }
}
```

- [ ] **Step 4: Run them and watch them fail**

Run: `cargo test --locked --lib bench::codebase::exec::tests`
Expected: FAIL to compile — `cannot find function run_cargo in module super`.

- [ ] **Step 5: Write the runner, the probe and the environment**

Above the test module in `src/core/bench/codebase/exec.rs`:

```rust
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::error::ChekovError;

/// How long each of the two cargo invocations may take before its process
/// group is killed (spec §3, §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    pub check: Duration,
    pub test: Duration,
}

impl Timeouts {
    /// The spec's ceilings. Carried on `Env` rather than read from a constant
    /// at the call site so the integration test can lower them.
    pub const DEFAULT: Self = Self {
        check: Duration::from_secs(120),
        test: Duration::from_secs(300),
    };
}

/// One cargo invocation.
pub struct CargoRun<'a> {
    pub args: &'a [&'a str],
    pub cwd: &'a Path,
    pub target_dir: &'a Path,
    pub timeout: Duration,
}

/// What it did. `status` is `None` when a signal ended it — which, when
/// `timed_out` is set, is the kill below.
#[derive(Debug)]
pub struct CargoOutcome {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub secs: f64,
    pub timed_out: bool,
}

/// The program to run. `cargo` in production; the tests point this at a shell
/// script, so no unit test in this crate needs a Rust toolchain installed.
fn cargo_program() -> std::ffi::OsString {
    std::env::var_os("CHEKOV_CARGO").unwrap_or_else(|| "cargo".into())
}

/// Spawn, drain both pipes on their own threads, poll the clock, kill the
/// group at the deadline.
pub fn run_cargo(run: &CargoRun) -> Result<CargoOutcome, ChekovError> {
    use std::os::unix::process::CommandExt;
    let started = Instant::now();
    let mut child = Command::new(cargo_program())
        .args(run.args)
        .current_dir(run.cwd)
        .env("CARGO_TARGET_DIR", run.target_dir)
        .env("CARGO_TERM_COLOR", "never")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|e| ChekovError::io(format!("spawning cargo {:?}", run.args), e))?;
    let out = drain(child.stdout.take());
    let err = drain(child.stderr.take());
    let timed_out = wait_or_kill(&mut child, run.timeout);
    let status = child.wait().ok().and_then(|s| s.code());
    Ok(CargoOutcome {
        status,
        stdout: out.join().unwrap_or_default(),
        stderr: err.join().unwrap_or_default(),
        secs: started.elapsed().as_secs_f64(),
        timed_out,
    })
}

/// Read one pipe to the end on its own thread.
///
/// `cargo check --message-format=json` writes megabytes. A `try_wait` loop
/// that never reads would wedge the child on a full pipe buffer, and the
/// "timeout" it then reported would be chekov's own deadlock.
fn drain<R: std::io::Read + Send + 'static>(pipe: Option<R>) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let Some(mut pipe) = pipe else {
            return String::new();
        };
        let mut buffer = Vec::new();
        let _ = std::io::Read::read_to_end(&mut pipe, &mut buffer);
        String::from_utf8_lossy(&buffer).into_owned()
    })
}

/// `true` when the deadline expired and the group was killed.
fn wait_or_kill(child: &mut Child, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => return false,
            Ok(None) => {}
        }
        if Instant::now() >= deadline {
            kill_group(child.id());
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// SIGKILL the child's whole process group.
///
/// `process_group(0)` made the child its own group leader, so its pgid IS its
/// pid and a negative pid reaches every rustc `cargo` spawned. `nix` is
/// already a dependency for exactly this call (`core/server.rs`); no `libc`,
/// and no shelling out to `kill(1)`.
fn kill_group(pid: u32) {
    let Ok(raw) = i32::try_from(pid) else {
        return;
    };
    let _ = nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(-raw),
        nix::sys::signal::Signal::SIGKILL,
    );
}

/// `Cargo.toml` at the root and a `cargo` that answers `--version`, or the
/// reason there is no toolchain.
///
/// A missing toolchain is a capability of the machine, never a failing score,
/// so this returns the reason every crossing will record rather than an error
/// that would stop the run.
pub fn probe(root: &Path) -> Result<String, String> {
    if !root.join("Cargo.toml").is_file() {
        return Err("no Rust toolchain: no Cargo.toml at the repository root".to_owned());
    }
    match Command::new(cargo_program()).arg("--version").output() {
        Ok(out) if out.status.success() => {
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
        }
        Ok(out) => Err(format!(
            "no Rust toolchain: cargo --version failed ({})",
            String::from_utf8_lossy(&out.stderr).trim()
        )),
        Err(e) => Err(format!("no Rust toolchain: cargo is not runnable ({e})")),
    }
}

/// The worktree, the scratch target directory and the toolchain the exec
/// tiers run in.
pub struct Env {
    pub worktree: super::tree::Worktree,
    pub target_dir: PathBuf,
    pub cargo_version: String,
    pub timeouts: Timeouts,
}

impl Env {
    /// The target directory, then the worktree — both explicit, so a cleanup
    /// failure is reported instead of being swallowed by `Worktree::drop`.
    pub fn finish(self) -> Result<(), ChekovError> {
        if self.target_dir.exists() {
            std::fs::remove_dir_all(&self.target_dir).map_err(|e| {
                ChekovError::io(format!("removing {}", self.target_dir.display()), e)
            })?;
        }
        self.worktree.remove()
    }
}

/// Whether the exec tiers run, and if not, why not.
pub enum Exec {
    /// `--allow-exec` was not given. Nothing was built and nothing was kept.
    Off,
    /// The flag was given and the machine cannot honour it — the reason every
    /// crossing records, once in the header.
    Unavailable(String),
    Ready(Env),
}

impl Exec {
    #[must_use]
    pub const fn allowed(&self) -> bool {
        !matches!(self, Self::Off)
    }

    #[must_use]
    pub fn cargo_version(&self) -> Option<&str> {
        match self {
            Self::Ready(env) => Some(env.cargo_version.as_str()),
            _ => None,
        }
    }

    #[must_use]
    pub const fn env(&self) -> Option<&Env> {
        match self {
            Self::Ready(env) => Some(env),
            _ => None,
        }
    }

    /// Remove what a ready environment is holding; the other two hold nothing.
    pub fn finish(self) -> Result<(), ChekovError> {
        match self {
            Self::Ready(env) => env.finish(),
            Self::Off | Self::Unavailable(_) => Ok(()),
        }
    }
}

/// The probe, the scratch target directory, and one online `cargo fetch`.
///
/// The worktree is CONSUMED: a ready environment keeps it for the run, and an
/// unavailable one removes it here, so the lifetime question has exactly one
/// answer per outcome.
pub fn prepare_env(
    worktree: super::tree::Worktree,
    scratch_root: &Path,
    head12: &str,
) -> Result<Exec, ChekovError> {
    let version = match probe(&worktree.path) {
        Ok(version) => version,
        Err(reason) => {
            worktree.remove()?;
            return Ok(Exec::Unavailable(reason));
        }
    };
    let target_dir = scratch_root.join(format!("target-{head12}"));
    std::fs::create_dir_all(&target_dir)
        .map_err(|e| ChekovError::io(format!("creating {}", target_dir.display()), e))?;
    fetch(&worktree.path, &target_dir);
    Ok(Exec::Ready(Env {
        worktree,
        target_dir,
        cargo_version: version,
        timeouts: Timeouts::DEFAULT,
    }))
}

/// The one invocation allowed the network, before the loop.
///
/// Its failure is not fatal: every later crossing carries `--offline`, and a
/// check that then needs the network records `needs network` with cargo's own
/// words — a per-crossing skip is more informative than refusing the run.
fn fetch(worktree: &Path, target_dir: &Path) {
    let outcome = run_cargo(&CargoRun {
        args: &["fetch"],
        cwd: worktree,
        target_dir,
        timeout: Timeouts::DEFAULT.check,
    });
    match outcome {
        Ok(out) if out.status == Some(0) => {}
        Ok(out) => eprintln!(
            "chekov bench: `cargo fetch` did not succeed ({}) — the exec tiers run offline \
             from here, and a crossing that needs the registry is skipped with cargo's reason",
            out.stderr.lines().next().unwrap_or("no output").trim()
        ),
        Err(e) => eprintln!("chekov bench: `cargo fetch` could not run ({e})"),
    }
}
```

- [ ] **Step 6: Run the tests and watch them pass**

Run: `cargo test --locked --lib bench::codebase::exec::tests -- --test-threads=1`
Expected: PASS (7 passed). `--test-threads=1` is belt-and-braces beside the `CARGO_ENV` mutex; the tests must also pass without it.

Run: `cargo test --locked --lib bench::codebase::exec::tests`
Expected: PASS (7 passed).

- [ ] **Step 7: Run the floor and commit**

Run: `cargo fmt && cargo clippy --locked --all-targets -- -D warnings && cargo test --locked`
Expected: clean; all tests pass.

```bash
git add src/core/bench/codebase/exec.rs src/core/bench/codebase/mod.rs src/core/bench/codebase/tree.rs && git commit -m "$(cat <<'EOF'
feat(codebase): the exec module's subprocess floor — probe, scratch target, timed cargo with a process-group kill

Both pipes are drained on their own threads: a try_wait loop that never
reads wedges the child it is timing as soon as the pipe buffer fills, and
the timeout it then reports is chekov's own deadlock. Every unit test
points $CHEKOV_CARGO at a shell script, so none of them needs a toolchain.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01YDaTTHJySW2ix3P85BcP6W
EOF
)"
```

---

### Task 3: `exec.rs` part 2 — the span in the original file, the splice, the diagnostics, the revert

The task's `byte_range` has to exist before anything can be spliced, so the `filter`/`mod` plumbing that computes it lives here with the splice that consumes it.

**Files:**
- Modify: `src/core/bench/codebase/filter.rs:28-33` (`Elided`), `:45-66` (`elide_cfg_test`), `:140-149` (`Context`), `:153-180` (`assemble`), plus new `original_range`
- Modify: `src/core/bench/codebase/mod.rs:76-97` (`CodebaseTask`), `:145-178` (`Elisions`, `elide_tests`), `:379-400` (`assembled_tasks`)
- Modify: `src/core/bench/codebase/ladder.rs:290` (`trimmed_to_gold` becomes `pub(super)`)
- Modify: `src/core/bench/codebase/exec.rs` (splice, diagnostics, network sniff, revert)
- Modify: `src/core/bench/codebase/run.rs:438-458` (`codebase_task_fixture`), `:492-516` (`cross_task`)
- Modify: `src/commands/capability.rs` (any `CodebaseTask { … }` literal the compiler names)

**Interfaces:**
- Consumes: `Timeouts`, `Env`, `run_cargo`, `CargoRun` (Task 2); `tree::git` (Task 2).
- Produces:
  ```rust
  // codebase/filter.rs
  pub struct Elided { pub text: String, pub lines_removed: usize, pub cuts: Vec<Range<usize>> }
  pub fn original_range(cuts: &[Range<usize>], elided: &Range<usize>) -> Range<usize>;
  pub struct Context<'a> {
      pub text: &'a str,
      pub cfg_test_lines: usize,
      pub cuts: &'a [Range<usize>],
      pub cross: Option<&'a super::crossfile::Assembled>,
  }

  // codebase/mod.rs
  pub struct CodebaseTask { /* … */ pub byte_range: std::ops::Range<usize> }

  // codebase/ladder.rs
  pub(super) fn trimmed_to_gold(gold: &str, prediction: &str) -> String;

  // codebase/exec.rs
  pub struct Splice<'a> { pub path: &'a Path, pub original: &'a str, pub span: Range<usize> }
  pub fn spliced(splice: &Splice, fill: &str) -> String;
  pub fn apply(splice: &Splice, fill: &str) -> Result<(), ChekovError>;
  pub fn first_error(stdout: &str) -> Option<String>;
  pub fn needs_network(stderr: &str) -> Option<String>;
  pub fn revert(env: &Env, file: &str, original: &str) -> Result<(), ChekovError>;
  ```

- [ ] **Step 1: Write the failing test for the original-coordinate range**

In `src/core/bench/codebase/filter.rs`'s `mod tests`, add:

```rust
    /// The elided text drops whole `#[cfg(test)]` regions, so an offset into
    /// it is short by every cut before it. Tier 6 splices into the file as it
    /// sits on disk — test modules intact, because tier 7 runs them — so the
    /// range the task carries has to be the original's.
    #[test]
    fn a_span_after_a_cut_maps_back_onto_the_original_bytes() {
        let original = "#[cfg(test)]\nmod a {\n    fn t() {}\n}\n\nfn keep() {\n    let x = 1;\n}\n";
        let elided = super::elide_cfg_test(original);
        assert!(!elided.text.contains("cfg(test)"), "{}", elided.text);
        let at = elided.text.find("let x = 1;").expect("the span survives the cut");
        let span = at..at + "let x = 1;".len();
        let mapped = super::original_range(&elided.cuts, &span);
        assert_eq!(&original[mapped], "let x = 1;");
    }

    /// A cut that begins exactly where the span ends is NOT inside the span.
    #[test]
    fn a_cut_starting_at_the_spans_end_stays_outside_it() {
        let original = "fn keep() {\n    let x = 1;\n}\n#[cfg(test)]\nmod a {\n    fn t() {}\n}\n";
        let elided = super::elide_cfg_test(original);
        let at = elided.text.find("let x = 1;").expect("the span survives");
        let span = at..at + "let x = 1;".len();
        let mapped = super::original_range(&elided.cuts, &span);
        assert_eq!(&original[mapped], "let x = 1;");
    }

    /// A file with no test module maps one-to-one.
    #[test]
    fn a_file_with_nothing_cut_maps_identically() {
        let original = "fn keep() {\n    let x = 1;\n}\n";
        let elided = super::elide_cfg_test(original);
        assert!(elided.cuts.is_empty());
        assert_eq!(super::original_range(&elided.cuts, &(12..22)), 12..22);
    }
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test --locked --lib bench::codebase::filter::tests`
Expected: FAIL — `no field cuts on type Elided`.

- [ ] **Step 3: Record the cuts and map back**

In `src/core/bench/codebase/filter.rs`, on `Elided` (`:29`):

```rust
/// A file's text with its `#[cfg(test)]` items removed, and what that cost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Elided {
    pub text: String,
    pub lines_removed: usize,
    /// The regions removed, in ORIGINAL coordinates, ascending and
    /// non-overlapping. Tier 6 splices into the file on disk, so a span found
    /// in `text` has to be mapped back through these.
    pub cuts: Vec<Range<usize>>,
}
```

In `elide_cfg_test` (`:45`), keep the cuts:

```rust
pub fn elide_cfg_test(text: &str) -> Elided {
    let cuts = cfg_test_cuts(text);
    if cuts.is_empty() {
        return Elided {
            text: text.to_owned(),
            lines_removed: 0,
            cuts,
        };
    }
    let mut kept = String::with_capacity(text.len());
    let mut lines_removed = 0;
    let mut from = 0;
    for cut in &cuts {
        kept.push_str(&text[from..cut.start]);
        lines_removed += text[cut.clone()].matches('\n').count();
        from = cut.end;
    }
    kept.push_str(&text[from..]);
    Elided {
        text: kept,
        lines_removed,
        cuts,
    }
}
```

Below it:

```rust
/// A span found in the elided text, as a span of the original.
///
/// The start shifts past a cut that begins at or before it — the cut's bytes
/// were removed there, so the span's text begins after them. The end shifts
/// only past a cut that begins strictly before it, or a cut abutting the
/// span's end would be swallowed into the span.
#[must_use]
pub fn original_range(cuts: &[Range<usize>], elided: &Range<usize>) -> Range<usize> {
    shift(cuts, elided.start, |start, at| start <= at)..shift(cuts, elided.end, |start, at| start < at)
}

fn shift(cuts: &[Range<usize>], at: usize, precedes: fn(usize, usize) -> bool) -> usize {
    let mut out = at;
    for cut in cuts {
        if !precedes(cut.start, out) {
            break;
        }
        out += cut.end - cut.start;
    }
    out
}
```

- [ ] **Step 4: Run them and watch them pass**

Run: `cargo test --locked --lib bench::codebase::filter::tests`
Expected: PASS (the three new ones plus the existing filter tests).

- [ ] **Step 5: Write the failing test that a task carries its original range**

In `src/core/bench/codebase/mod.rs`'s `mod tests`, add:

```rust
    /// Every assembled task's `byte_range` indexes the file as it sits in the
    /// worktree — test modules intact — and lands exactly on the gold.
    #[test]
    fn every_tasks_byte_range_indexes_the_worktrees_own_file() {
        let (repo, prepared) = prepared_fixture("byte-range");
        assert!(!prepared.tasks.is_empty(), "the fixture yields tasks");
        for task in &prepared.tasks {
            let original =
                std::fs::read_to_string(repo.join(&task.file)).expect("the file on disk");
            assert_eq!(
                &original[task.byte_range.clone()],
                task.gold,
                "task {} in {}",
                task.id,
                task.file
            );
        }
    }
```

`prepared_fixture(name) -> (PathBuf, Prepared)` is a helper this task adds beside the existing `git`/`source` helpers (`mod.rs:440-459`): it builds a temp repo from `source("alpha")`/`source("beta")`, commits it, runs `prepare`, and returns **the repo path** (not the worktree, which `prepare` removes) together with the `Prepared`. Since the worktree is a detached checkout of the same HEAD, the repo's own files are byte-identical to the ones the task was cut from.

```rust
    /// A committed two-file repo and the `Prepared` sampled from it. The repo
    /// path comes back because the worktree is gone by then, and a clean
    /// checkout of the same HEAD has byte-identical files.
    fn prepared_fixture(name: &str) -> (PathBuf, Prepared) {
        let root = std::env::temp_dir().join("chekov-test-codebase").join(name);
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join("src")).expect("src dir");
        std::fs::write(repo.join("src/alpha.rs"), source("alpha")).expect("alpha");
        std::fs::write(repo.join("src/beta.rs"), source("beta")).expect("beta");
        git(&repo, &["init", "-q"]);
        git(&repo, &["config", "user.email", "t@t"]);
        git(&repo, &["config", "user.name", "t"]);
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "fixture"]);
        let prepared = prepare(
            &repo,
            &crate::core::bench::codebase::PrepareInputs {
                scratch_root: &root.join("scratch"),
                tasks: 8,
                allow_exec: false,
            },
        )
        .expect("prepare");
        (repo, prepared)
    }
```

> `PrepareInputs` is introduced in Task 5. Until then, write this helper's `prepare` call in the current three-argument shape — `prepare(&repo, &root.join("scratch"), 8)` — and Task 5's step that introduces `PrepareInputs` updates it. The rest of the test is unchanged either way.

- [ ] **Step 6: Run it and watch it fail**

Run: `cargo test --locked --lib bench::codebase::tests::every_tasks_byte_range_indexes_the_worktrees_own_file`
Expected: FAIL — `no field byte_range on type CodebaseTask`.

- [ ] **Step 7: Add the field and plumb the cuts through**

In `src/core/bench/codebase/mod.rs`, on `CodebaseTask` after `line` (`:82`):

```rust
    /// The span in the file as it sits in the worktree — test modules intact.
    ///
    /// `masker::Candidate.byte_range` indexes the ELIDED text; tier 6 splices
    /// into the original, because tier 7 runs the very test modules elision
    /// cut. `filter::original_range` is the map between them, and the
    /// invariant is `&original[byte_range] == gold`.
    pub byte_range: std::ops::Range<usize>,
```

Replace `Elisions`'s per-file map (`:145-178`) so it keeps the cuts:

```rust
/// What one file's `#[cfg(test)]` cut cost, and where it fell.
struct FileElision {
    lines: usize,
    cuts: Vec<std::ops::Range<usize>>,
}

/// Every walked file with its `#[cfg(test)]` items already cut, keyed back to
/// what each cut cost so a task's row can carry its own file's number — and
/// to WHERE each cut fell, so a span can be mapped onto the original.
struct Elisions {
    files: Vec<(String, String)>,
    per_file: std::collections::HashMap<String, FileElision>,
}

impl Elisions {
    fn lines(&self) -> usize {
        self.per_file.values().map(|e| e.lines).sum()
    }

    fn files_cut(&self) -> usize {
        self.per_file.values().filter(|e| e.lines > 0).count()
    }

    /// One file's cut list, or an empty one for a file that gave nothing up.
    fn cuts(&self, path: &str) -> &[std::ops::Range<usize>] {
        self.per_file.get(path).map_or(&[], |e| e.cuts.as_slice())
    }

    fn lines_of(&self, path: &str) -> usize {
        self.per_file.get(path).map_or(0, |e| e.lines)
    }
}

fn elide_tests(files: Vec<(String, String)>) -> Elisions {
    let mut per_file = std::collections::HashMap::new();
    let files = files
        .into_iter()
        .map(|(path, text)| {
            let cut = filter::elide_cfg_test(&text);
            per_file.insert(
                path.clone(),
                FileElision {
                    lines: cut.lines_removed,
                    cuts: cut.cuts,
                },
            );
            (path, cut.text)
        })
        .collect();
    Elisions { files, per_file }
}
```

In `assembled_tasks` (`:379`), replace the `cfg_test_lines` lookup and add the cuts:

```rust
            Some(filter::assemble(
                p,
                &filter::Context {
                    text,
                    cfg_test_lines: a.elided.lines_of(&p.path),
                    cuts: a.elided.cuts(&p.path),
                    cross: cross_for(p, a, text).as_ref(),
                },
            ))
```

In `src/core/bench/codebase/filter.rs`, on `Context` (`:141`) add:

```rust
    /// Where that file's cuts fell, so the span can be mapped onto the file
    /// as it sits on disk.
    pub cuts: &'a [Range<usize>],
```

and in `assemble` (`:154`), after `line`:

```rust
        byte_range: original_range(ctx.cuts, &c.byte_range),
```

- [ ] **Step 8: Fix every `CodebaseTask { … }` literal the compiler names**

Run: `cargo check --locked --all-targets`
Expected: FAIL with `missing field byte_range` at `run.rs`'s `codebase_task_fixture` (`:439`) and `cross_task` (`:493`), and at any fixture in `commands/capability.rs`. Give each a range that matches its own gold, e.g. in `codebase_task_fixture` (gold `"let a = 1;"`, prefix `"fn f() {\n"`):

```rust
            byte_range: 9..19,
```

and in `cross_task` (gold `"let a = build(1);"`, prefix `"pub fn run() {\n"`):

```rust
            byte_range: 15..32,
```

- [ ] **Step 9: Run the codebase tests and watch them pass**

Run: `cargo test --locked --lib bench::codebase::`
Expected: PASS, including `every_tasks_byte_range_indexes_the_worktrees_own_file`.

- [ ] **Step 10: Widen `trimmed_to_gold`**

In `src/core/bench/codebase/ladder.rs:290`, change `fn trimmed_to_gold(` to `pub(super) fn trimmed_to_gold(` and add one line to its doc:

```rust
/// The first `gold.lines().count()` lines of the prediction, ending the way
/// the gold ends.
///
/// `pub(super)` for tier 6: the text that gets spliced into the file and
/// compiled is the same text tiers 1-4 grade. A compile verdict on a
/// different string from the one the report scores would be two answers to
/// one question.
```

- [ ] **Step 11: Write the failing tests for the splice, the diagnostics and the network sniff**

In `src/core/bench/codebase/exec.rs`'s `mod tests`, add:

```rust
    fn splice_of<'a>(path: &'a Path, original: &'a str, span: std::ops::Range<usize>)
        -> super::Splice<'a> {
        super::Splice { path, original, span }
    }

    #[test]
    fn a_splice_replaces_the_span_and_leaves_every_other_byte_alone() {
        let original = "fn f() {\n    let a = 1;\n}\n\n#[cfg(test)]\nmod t {\n    fn q() {}\n}\n";
        let at = original.find("let a = 1;").expect("the span");
        let out = super::spliced(
            &splice_of(Path::new("/nowhere"), original, at..at + 10),
            "let a = 2;",
        );
        assert_eq!(out, original.replace("let a = 1;", "let a = 2;"));
        assert!(out.contains("#[cfg(test)]"), "the test module is intact: {out}");
    }

    #[test]
    fn a_span_at_byte_zero_and_a_span_at_eof_both_splice() {
        let original = "abcdef";
        let head = super::spliced(&splice_of(Path::new("/n"), original, 0..3), "XY");
        assert_eq!(head, "XYdef");
        let tail = super::spliced(&splice_of(Path::new("/n"), original, 6..6), "Z");
        assert_eq!(tail, "abcdefZ");
    }

    #[test]
    fn the_first_error_wins_and_warnings_are_ignored() {
        let stream = concat!(
            r#"{"reason":"compiler-artifact","package_id":"x"}"#,
            "\n",
            r#"{"reason":"compiler-message","message":{"level":"warning","message":"unused",
               "spans":[{"file_name":"src/a.rs","line_start":3,"is_primary":true}]}}"#,
            "\n",
            r#"{"reason":"compiler-message","message":{"level":"error","message":"mismatched types",
               "spans":[{"file_name":"src/b.rs","line_start":42,"is_primary":true}]}}"#,
            "\n",
        );
        assert_eq!(
            super::first_error(stream).as_deref(),
            Some("src/b.rs:42: mismatched types")
        );
    }

    #[test]
    fn warnings_alone_are_a_pass_and_malformed_lines_are_ignored() {
        let stream = concat!(
            "warning: this line is not JSON at all\n",
            "{ not json either\n",
            r#"{"reason":"compiler-message","message":{"level":"warning","message":"unused","spans":[]}}"#,
            "\n",
        );
        assert_eq!(super::first_error(stream), None);
    }

    /// A fill can break a caller in another file — that IS the point of the
    /// cross-file tier — so an error anywhere in the workspace counts.
    #[test]
    fn an_error_in_another_file_still_counts() {
        let stream = concat!(
            r#"{"reason":"compiler-message","message":{"level":"error","message":"no method `zap`",
               "spans":[{"file_name":"src/caller.rs","line_start":9,"is_primary":true}]}}"#,
            "\n",
        );
        assert_eq!(
            super::first_error(stream).as_deref(),
            Some("src/caller.rs:9: no method `zap`")
        );
    }

    /// An error with no primary span is still an error.
    #[test]
    fn an_error_without_a_primary_span_keeps_its_message() {
        let stream = r#"{"reason":"compiler-message","message":{"level":"error","message":"linking failed","spans":[]}}"#;
        assert_eq!(super::first_error(stream).as_deref(), Some("linking failed"));
    }

    #[test]
    fn cargos_offline_complaint_is_recognised_and_quoted() {
        let stderr = "    Updating crates.io index\nerror: no matching package named `serde` \
                      found\nperhaps you meant to use --offline\n";
        let found = super::needs_network(stderr).expect("cargo said it needed the registry");
        assert!(found.contains("no matching package named"), "{found}");
        assert_eq!(super::needs_network("error: mismatched types\n"), None);
    }
```

- [ ] **Step 12: Run them and watch them fail**

Run: `cargo test --locked --lib bench::codebase::exec::tests`
Expected: FAIL — `cannot find function spliced in module super`.

- [ ] **Step 13: Write the splice, the diagnostics parser, the network sniff and the revert**

Append to the production half of `src/core/bench/codebase/exec.rs`:

```rust
/// One crossing's edit: which file, what it held, and the bytes to replace.
pub struct Splice<'a> {
    pub path: &'a Path,
    pub original: &'a str,
    pub span: std::ops::Range<usize>,
}

/// `original` with `span` replaced by `fill`, every other byte identical —
/// test modules included, because tier 7 runs them.
#[must_use]
pub fn spliced(splice: &Splice, fill: &str) -> String {
    let mut out = String::with_capacity(splice.original.len() + fill.len());
    out.push_str(&splice.original[..splice.span.start]);
    out.push_str(fill);
    out.push_str(&splice.original[splice.span.end..]);
    out
}

/// The splice, written. No other file in the worktree is touched.
pub fn apply(splice: &Splice, fill: &str) -> Result<(), ChekovError> {
    std::fs::write(splice.path, spliced(splice, fill))
        .map_err(|e| ChekovError::io(format!("writing {}", splice.path.display()), e))
}

/// The first `error` diagnostic in a `--message-format=json` stream, as
/// `<file>:<line>: <message>`.
///
/// Warnings are ignored — a fill that compiles with warnings compiles — and a
/// line that is not JSON is ignored, because cargo interleaves plain progress
/// text on the same stream. The diagnostics, not the exit status, are the
/// verdict: cargo exits non-zero for things it also reports, and the stream is
/// the auditable record.
#[must_use]
pub fn first_error(stdout: &str) -> Option<String> {
    stdout.lines().find_map(error_line)
}

fn error_line(line: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    let message = value.get("message")?;
    if message.get("level")?.as_str()? != "error" {
        return None;
    }
    let text = message.get("message")?.as_str()?;
    Some(match primary_span(message) {
        Some((file, at)) => format!("{file}:{at}: {text}"),
        None => text.to_owned(),
    })
}

fn primary_span(message: &serde_json::Value) -> Option<(String, u64)> {
    let span = message
        .get("spans")?
        .as_array()?
        .iter()
        .find(|s| s.get("is_primary").and_then(serde_json::Value::as_bool) == Some(true))?;
    Some((
        span.get("file_name")?.as_str()?.to_owned(),
        span.get("line_start")?.as_u64()?,
    ))
}

/// cargo's own line when `--offline` is what stopped it, or `None`.
///
/// This is never retried online: the run fetched once before the loop, and a
/// crossing that still wants the registry is a skip with cargo's words, not a
/// second trip to the network mid-benchmark.
#[must_use]
pub fn needs_network(stderr: &str) -> Option<String> {
    const MARKERS: [&str; 4] = [
        "--offline",
        "failed to download",
        "no matching package named",
        "unable to get packages from source",
    ];
    stderr
        .lines()
        .map(str::trim)
        .find(|line| MARKERS.iter().any(|marker| line.contains(marker)))
        .map(str::to_owned)
}

/// `git checkout -- F`, then the bytes back.
///
/// A revert that does not restore aborts the run: every later crossing would
/// be measured against a file nobody can vouch for, and a benchmark that
/// cannot say what it compiled has measured nothing.
pub fn revert(env: &Env, file: &str, original: &str) -> Result<(), ChekovError> {
    super::tree::git(
        &env.worktree.path,
        &["checkout", "--", file],
        "git checkout (undo the tier-6 splice)",
    )?;
    let path = env.worktree.path.join(file);
    let now = std::fs::read_to_string(&path)
        .map_err(|e| ChekovError::io(format!("re-reading {}", path.display()), e))?;
    if now == original {
        return Ok(());
    }
    Err(ChekovError::ExecWorktreeDirty {
        path: env.worktree.path.clone(),
        file: file.to_owned(),
    })
}
```

- [ ] **Step 14: Run them and watch them pass**

Run: `cargo test --locked --lib bench::codebase::exec::tests`
Expected: PASS (14 passed).

- [ ] **Step 15: Write the failing test for the revert against a real git worktree**

In `src/core/bench/codebase/exec.rs`'s `mod tests`, add:

```rust
    /// A worktree the revert restores, and one it cannot: the second is the
    /// abort, and it names the file.
    #[test]
    fn a_revert_restores_the_file_and_a_failure_to_restore_aborts() {
        let dir = scratch("revert");
        let repo = dir.join("repo");
        std::fs::create_dir_all(repo.join("src")).expect("src");
        std::fs::write(repo.join("Cargo.toml"), "[package]\nname = \"x\"\n").expect("manifest");
        std::fs::write(repo.join("src/a.rs"), "fn f() {\n    let a = 1;\n}\n").expect("a.rs");
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "t@t"],
            vec!["config", "user.name", "t"],
            vec!["add", "-A"],
            vec!["commit", "-qm", "fixture"],
        ] {
            super::super::tree::git(&repo, &args, "fixture").expect("git");
        }
        let worktree =
            super::super::tree::Worktree::add(&repo, &dir.join("tree")).expect("worktree");
        let original =
            std::fs::read_to_string(worktree.path.join("src/a.rs")).expect("the original");
        let env = super::Env {
            worktree,
            target_dir: dir.join("target"),
            cargo_version: "cargo 1.95.0".to_owned(),
            timeouts: Timeouts::DEFAULT,
        };
        let path = env.worktree.path.join("src/a.rs");
        super::apply(
            &super::Splice {
                path: &path,
                original: &original,
                span: 9..19,
            },
            "let a = 2;",
        )
        .expect("apply");
        assert_ne!(
            std::fs::read_to_string(&path).expect("read"),
            original,
            "the splice landed"
        );
        super::revert(&env, "src/a.rs", &original).expect("the revert restores");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), original);

        // A file whose committed content is not what we claim it was: the
        // checkout succeeds and the bytes still differ, which is the abort.
        let wrong = format!("{original}// drifted\n");
        let err = super::revert(&env, "src/a.rs", &wrong)
            .expect_err("a worktree that will not restore stops the run");
        let text = err.to_string();
        assert!(text.contains("src/a.rs"), "{text}");
        assert!(text.contains("--resume"), "{text}");
        env.finish().expect("cleanup");
    }
```

- [ ] **Step 16: Run it and watch it pass**

Run: `cargo test --locked --lib bench::codebase::exec::tests::a_revert_restores_the_file_and_a_failure_to_restore_aborts`
Expected: PASS. (It needs `git`, which the existing `codebase::tests` already require.)

- [ ] **Step 17: Run the floor and commit**

Run: `cargo fmt && cargo clippy --locked --all-targets -- -D warnings && cargo test --locked`
Expected: clean; all tests pass.

```bash
git add src/core/bench/codebase/exec.rs src/core/bench/codebase/filter.rs src/core/bench/codebase/mod.rs src/core/bench/codebase/ladder.rs src/core/bench/codebase/run.rs src/commands/capability.rs && git commit -m "$(cat <<'EOF'
feat(codebase): the span in the original file, the splice, the JSON diagnostics, and the verified revert

A task's byte_range now indexes the file as it sits on disk rather than
the elided text: tier 7 runs the very #[cfg(test)] modules elision cut,
so tier 6 cannot splice into a text without them. The map is exact
because the cutter only ever deletes whole ranges.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01YDaTTHJySW2ix3P85BcP6W
EOF
)"
```

---

### Task 4: `exec.rs` part 3 — the enclosing function, the crate, the covering tests, and the test run

**Files:**
- Modify: `src/core/bench/codebase/masker.rs` (new `enclosing_fn`, beside the private `fn_signatures`/`body_after`)
- Modify: `src/core/bench/codebase/exec.rs` (`Crate`, `crate_of`, `crate_rust_files`, `covering_tests`, `TestRun`, `TestVerdict`, `run_tests`)

**Interfaces:**
- Consumes: `masker::matching_close(text, open) -> Option<usize>` (`masker.rs:175`, already `pub(crate)`); `ladder::code_only(text) -> String` (`ladder.rs:412`, already `pub(super)`); `Env`, `run_cargo`, `CargoRun`, `needs_network` (Tasks 2-3).
- Produces:
  ```rust
  // codebase/masker.rs
  pub(super) fn enclosing_fn(text: &str, at: usize) -> Option<String>;

  // codebase/exec.rs
  pub struct Crate { pub name: String, pub root: PathBuf }
  pub fn crate_of(worktree: &Path, file: &str) -> Option<Crate>;
  pub fn covering_tests(root: &Path, symbols: &[String]) -> Vec<String>;

  pub struct TestRun<'a> { pub env: &'a Env, pub krate: &'a str, pub tests: &'a [String] }
  pub enum TestVerdict { Passed, Failed(String), Skipped(String) }
  pub fn run_tests(run: &TestRun) -> (TestVerdict, f64);

  pub const CAP: usize = 5;
  ```

**Why exec.rs walks the crate itself instead of reusing `tree::rust_sources`:** that walk deliberately skips the `tests/` directory and test-named files (`tree.rs:160`, `:171`) — it is building the *task* set, and masking an assertion measures nothing. Tier 7 wants exactly the files that walk throws away: a covering test very often lives in `tests/*.rs`. Two walks, two jobs.

- [ ] **Step 1: Write the failing test for the enclosing function**

In `src/core/bench/codebase/masker.rs`'s `mod tests`, add:

```rust
    #[test]
    fn the_enclosing_function_of_a_span_is_the_innermost_one() {
        let text = "fn outer() {\n    let a = 1;\n}\n\nfn inner() {\n    let b = 2;\n}\n";
        let at = text.find("let b = 2;").expect("the span");
        assert_eq!(super::enclosing_fn(text, at).as_deref(), Some("inner"));
        let at = text.find("let a = 1;").expect("the span");
        assert_eq!(super::enclosing_fn(text, at).as_deref(), Some("outer"));
    }

    /// A nested `fn` wins over the one containing it: the span belongs to the
    /// body it actually sits in.
    #[test]
    fn a_nested_function_wins_over_the_one_around_it() {
        let text = "fn outer() {\n    fn helper() {\n        let x = 1;\n    }\n    helper();\n}\n";
        let at = text.find("let x = 1;").expect("the span");
        assert_eq!(super::enclosing_fn(text, at).as_deref(), Some("helper"));
    }

    /// A `const` or a `use` at file scope has no enclosing function, and the
    /// answer is `None` rather than the nearest one above it.
    #[test]
    fn a_span_outside_every_body_has_no_enclosing_function() {
        let text = "fn f() {\n    let a = 1;\n}\n\nconst K: u8 = 3;\n";
        let at = text.find("const K").expect("the span");
        assert_eq!(super::enclosing_fn(text, at), None);
    }
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test --locked --lib bench::codebase::masker::tests`
Expected: FAIL — `cannot find function enclosing_fn in module super`.

- [ ] **Step 3: Write `enclosing_fn`**

In `src/core/bench/codebase/masker.rs`, after `body_after` (`:170`):

```rust
/// The name of the function whose body contains `at`, innermost first.
///
/// Tier 7 needs a symbol to look for in the repository's tests, and for
/// `function_body` that is the masked fn itself while for `in_file` and
/// `cross_file_first` it is whatever fn the statement sits in. One scan
/// answers both. A span outside every body — a `const`, a `use`, an item
/// attribute — has no enclosing fn, and `None` is the honest answer: tier 7
/// records `no enclosing function` rather than guessing at the fn above.
pub(super) fn enclosing_fn(text: &str, at: usize) -> Option<String> {
    let mut innermost: Option<(usize, &str)> = None;
    for sig in fn_signatures(text) {
        let Some(body) = body_after(text, sig.end) else {
            continue;
        };
        if !body.contains(&at) {
            continue;
        }
        if innermost.is_none_or(|(start, _)| body.start > start) {
            innermost = Some((body.start, &text[sig.start + 3..sig.end]));
        }
    }
    innermost.map(|(_, name)| name.trim().to_owned())
}
```

(`sig` is the range of `fn` through the name, so `sig.start + 3` skips the keyword; `fn_signatures` guarantees the literal `fn ` at `sig.start`.)

- [ ] **Step 4: Run them and watch them pass**

Run: `cargo test --locked --lib bench::codebase::masker::tests`
Expected: PASS (the three new ones plus the existing masker tests).

- [ ] **Step 5: Write the failing tests for the crate lookup and covering-test discovery**

In `src/core/bench/codebase/exec.rs`'s `mod tests`, add:

```rust
    /// A tiny crate on disk: `Cargo.toml`, a `src/` file, and whatever else
    /// the test wants.
    fn crate_fixture(name: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir = scratch(name);
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"widget\"\nversion = \"0.1.0\"\n")
            .expect("manifest");
        for (path, text) in files {
            let full = dir.join(path);
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent).expect("parent");
            }
            std::fs::write(full, text).expect("file");
        }
        dir
    }

    #[test]
    fn the_crate_is_the_nearest_manifest_with_a_package_name() {
        let root = crate_fixture("crate-of", &[("src/deep/a.rs", "fn f() {}\n")]);
        let found = super::crate_of(&root, "src/deep/a.rs").expect("a crate");
        assert_eq!(found.name, "widget");
        assert_eq!(found.root, root);
    }

    /// A virtual workspace root has no `[package]`, so a file under one with
    /// no nearer manifest belongs to no crate.
    #[test]
    fn a_workspace_root_without_a_package_is_no_crate() {
        let dir = scratch("virtual-workspace");
        std::fs::create_dir_all(dir.join("src")).expect("src");
        std::fs::write(dir.join("Cargo.toml"), "[workspace]\nmembers = [\"a\"]\n")
            .expect("manifest");
        std::fs::write(dir.join("src/a.rs"), "fn f() {}\n").expect("a.rs");
        assert!(super::crate_of(&dir, "src/a.rs").is_none());
    }

    #[test]
    fn a_covering_test_is_found_inline_and_in_the_tests_directory() {
        let root = crate_fixture(
            "covering",
            &[
                (
                    "src/lib.rs",
                    "pub fn alpha() -> u8 { 1 }\npub fn beta() -> u8 { 2 }\n\n\
                     #[cfg(test)]\nmod t {\n    #[test]\n    fn covers_alpha() {\n        \
                     assert_eq!(super::alpha(), 1);\n    }\n    #[test]\n    fn covers_beta() \
                     {\n        assert_eq!(super::beta(), 2);\n    }\n}\n",
                ),
                (
                    "tests/outer.rs",
                    "#[test]\nfn integration_alpha() {\n    assert_eq!(widget::alpha(), 1);\n}\n",
                ),
            ],
        );
        let found = super::covering_tests(&root, &["alpha".to_owned()]);
        assert_eq!(found, vec!["covers_alpha".to_owned(), "integration_alpha".to_owned()]);
        assert!(super::covering_tests(&root, &["gamma".to_owned()]).is_empty());
    }

    /// `#[test]` with an attribute between it and the `fn` still counts.
    #[test]
    fn an_attribute_between_the_test_marker_and_the_fn_does_not_hide_it() {
        let root = crate_fixture(
            "adjacency",
            &[(
                "src/lib.rs",
                "pub fn alpha() {}\n#[test]\n#[ignore]\nfn covers_alpha() { alpha(); }\n",
            )],
        );
        assert_eq!(
            super::covering_tests(&root, &["alpha".to_owned()]),
            vec!["covers_alpha".to_owned()]
        );
    }

    /// A mention inside a string or a comment is prose, not a call.
    #[test]
    fn a_symbol_named_only_in_a_literal_or_a_comment_does_not_cover() {
        let root = crate_fixture(
            "prose",
            &[(
                "src/lib.rs",
                "pub fn alpha() {}\n#[test]\nfn mentions_alpha() {\n    // alpha is nice\n    \
                 let s = \"alpha\";\n    assert!(!s.is_empty());\n}\n",
            )],
        );
        assert!(super::covering_tests(&root, &["alpha".to_owned()]).is_empty());
    }

    /// Whole words only: `alphabet` is not `alpha`.
    #[test]
    fn a_longer_identifier_that_merely_contains_the_symbol_does_not_cover() {
        let root = crate_fixture(
            "whole-word",
            &[(
                "src/lib.rs",
                "pub fn alpha() {}\n#[test]\nfn t() {\n    let alphabet = 1;\n    \
                 assert_eq!(alphabet, 1);\n}\n",
            )],
        );
        assert!(super::covering_tests(&root, &["alpha".to_owned()]).is_empty());
    }

    #[test]
    fn the_candidates_stop_at_five_in_file_order() {
        let body = (0..8)
            .map(|i| format!("#[test]\nfn covers_{i}() {{ alpha(); }}\n"))
            .collect::<String>();
        let root = crate_fixture(
            "cap",
            &[("src/lib.rs", &format!("pub fn alpha() {{}}\n{body}"))],
        );
        let found = super::covering_tests(&root, &["alpha".to_owned()]);
        assert_eq!(found.len(), super::CAP);
        assert_eq!(found[0], "covers_0", "file order, not hash order");
        assert_eq!(found[4], "covers_4");
    }
```

- [ ] **Step 6: Run them and watch them fail**

Run: `cargo test --locked --lib bench::codebase::exec::tests`
Expected: FAIL — `cannot find function crate_of in module super`.

- [ ] **Step 7: Write the crate lookup and the discovery**

Append to the production half of `src/core/bench/codebase/exec.rs`:

```rust
/// At most this many covering tests are run for one crossing (spec §4).
///
/// Tier 7's question is whether the fill kept the code working, not how much
/// of the suite it survives; five is enough to answer it inside the timeout.
pub const CAP: usize = 5;

/// The crate a masked file belongs to.
pub struct Crate {
    pub name: String,
    pub root: PathBuf,
}

/// Only the two keys tier 7 needs, out of a manifest chekov does not own.
///
/// No `deny_unknown_fields` here, unlike every struct chekov defines the
/// schema for: this one reads someone else's file, and a manifest with a
/// `[dependencies]` table is not a schema error.
#[derive(serde::Deserialize)]
struct Manifest {
    package: Option<ManifestPackage>,
}

#[derive(serde::Deserialize)]
struct ManifestPackage {
    name: String,
}

/// The nearest `Cargo.toml` at or above `file` with a `[package] name`.
///
/// A virtual workspace root has no `[package]`, so a file with no nearer
/// manifest belongs to no crate and tier 7 records `no crate` — `-p` needs a
/// package name, and inventing one would run the wrong tests.
#[must_use]
pub fn crate_of(worktree: &Path, file: &str) -> Option<Crate> {
    let mut dir = worktree.join(file).parent()?.to_path_buf();
    loop {
        if let Some(name) = package_name(&dir.join("Cargo.toml")) {
            return Some(Crate { name, root: dir });
        }
        if dir == worktree || !dir.pop() {
            return None;
        }
    }
}

fn package_name(manifest: &Path) -> Option<String> {
    let text = std::fs::read_to_string(manifest).ok()?;
    let parsed: Manifest = toml::from_str(&text).ok()?;
    parsed.package.map(|p| p.name)
}

/// `#[test]` functions in the crate whose body mentions one of `symbols` as a
/// whole word outside literals and comments. File order, capped at `CAP`.
///
/// The walk covers `tests/` too: the task set's own walk skips it on purpose
/// (masking an assertion measures nothing), and that is exactly where an
/// integration test covering the masked symbol lives.
#[must_use]
pub fn covering_tests(root: &Path, symbols: &[String]) -> Vec<String> {
    let mut found = Vec::new();
    for (_, text) in crate_rust_files(root) {
        tests_in(&text, symbols, &mut found);
        if found.len() >= CAP {
            found.truncate(CAP);
            return found;
        }
    }
    found
}

/// Every `*.rs` under the crate — `tests/` included, `target/` and `.git/`
/// excluded — as `(relative path, text)`, sorted by path.
fn crate_rust_files(root: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    walk_crate(root, root, &mut out);
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn walk_crate(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if !matches!(name.as_str(), "target" | ".git") {
                walk_crate(root, &path, out);
            }
        } else {
            // `take_rs` answers `None` for a non-`.rs` file or an unreadable
            // one; neither is worth reporting, and the `Option` is `must_use`.
            let _ = take_rs(root, &path, out);
        }
    }
}

fn take_rs(root: &Path, path: &Path, out: &mut Vec<(String, String)>) -> Option<()> {
    if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("rs")) {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    let relative = path.strip_prefix(root).ok()?.to_string_lossy().into_owned();
    out.push((relative, text));
    Some(())
}

/// One file's covering tests, appended in source order.
fn tests_in(text: &str, symbols: &[String], out: &mut Vec<String>) {
    let code = super::ladder::code_only(text);
    for at in test_attribute_offsets(&code) {
        let Some((name, body)) = test_fn_after(&code, at) else {
            continue;
        };
        if symbols.iter().any(|s| mentions(&code[body], s)) {
            out.push(name);
        }
        if out.len() >= CAP {
            return;
        }
    }
}

/// Every offset of a `#[test]` attribute at the start of a line.
///
/// Read from the literal-blanked text, so `"#[test]"` inside a string is not
/// one. `#[test]` is the whole trimmed line — `#[test_case]` is a different
/// attribute and does not match.
fn test_attribute_offsets(code: &str) -> Vec<usize> {
    let mut offsets = Vec::new();
    let mut at = 0;
    for line in code.split_inclusive('\n') {
        if line.trim() == "#[test]" {
            offsets.push(at + line.len());
        }
        at += line.len();
    }
    offsets
}

/// The `fn <name>` below a `#[test]`, and the byte range of its body.
///
/// Attribute lines and blank lines between the two are stepped over — an
/// `#[ignore]` under the `#[test]` does not stop it being a test — and
/// anything else ends the search.
fn test_fn_after(code: &str, from: usize) -> Option<(String, std::ops::Range<usize>)> {
    let mut at = from;
    loop {
        let line = &code[at..code[at..].find('\n').map_or(code.len(), |i| at + i + 1)];
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("fn ") {
            let end = rest
                .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .unwrap_or(rest.len());
            let open = at + code[at..].find('{')?;
            let close = super::masker::matching_close(code, open)?;
            return Some((rest[..end].to_owned(), open + 1..close));
        }
        if !(trimmed.is_empty() || trimmed.starts_with('#')) {
            return None;
        }
        at += line.len();
        if at >= code.len() {
            return None;
        }
    }
}

/// `symbol` as a whole word somewhere in `code`.
fn mentions(code: &str, symbol: &str) -> bool {
    code.match_indices(symbol).any(|(at, _)| {
        let before = code[..at].chars().next_back();
        let after = code[at + symbol.len()..].chars().next();
        !before.is_some_and(word_char) && !after.is_some_and(word_char)
    })
}

fn word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}
```

- [ ] **Step 8: Run them and watch them pass**

Run: `cargo test --locked --lib bench::codebase::exec::tests`
Expected: PASS (21 passed).

- [ ] **Step 9: Write the failing test for the test runner**

In `src/core/bench/codebase/exec.rs`'s `mod tests`, add:

```rust
    fn env_for(dir: &Path, check: Duration, test: Duration) -> super::Env {
        super::Env {
            worktree: super::super::tree::Worktree::detached_for_test(dir.join("tree")),
            target_dir: dir.join("target"),
            cargo_version: "cargo 1.95.0".to_owned(),
            timeouts: Timeouts { check, test },
        }
    }

    #[test]
    fn every_candidate_must_pass_for_tier_seven_to_pass() {
        let guard = CARGO_ENV.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = scratch("tests-pass");
        std::fs::create_dir_all(dir.join("tree")).expect("tree");
        let cargo = fake_cargo(&dir, "exit 0");
        unsafe { std::env::set_var("CHEKOV_CARGO", &cargo) };
        let env = env_for(&dir, Duration::from_secs(5), Duration::from_secs(5));
        let (verdict, secs) = super::run_tests(&super::TestRun {
            env: &env,
            krate: "widget",
            tests: &["covers_alpha".to_owned(), "covers_beta".to_owned()],
        });
        unsafe { std::env::remove_var("CHEKOV_CARGO") };
        drop(guard);
        assert!(matches!(verdict, super::TestVerdict::Passed), "{verdict:?}");
        assert!(secs >= 0.0);
    }

    #[test]
    fn the_first_failing_candidate_is_named_with_cargos_text() {
        let guard = CARGO_ENV.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = scratch("tests-fail");
        std::fs::create_dir_all(dir.join("tree")).expect("tree");
        let cargo = fake_cargo(&dir, "echo 'test covers_alpha ... FAILED'\nexit 101");
        unsafe { std::env::set_var("CHEKOV_CARGO", &cargo) };
        let env = env_for(&dir, Duration::from_secs(5), Duration::from_secs(5));
        let (verdict, _) = super::run_tests(&super::TestRun {
            env: &env,
            krate: "widget",
            tests: &["covers_alpha".to_owned()],
        });
        unsafe { std::env::remove_var("CHEKOV_CARGO") };
        drop(guard);
        let super::TestVerdict::Failed(text) = verdict else {
            panic!("expected a failure, got {verdict:?}");
        };
        assert!(text.starts_with("covers_alpha: "), "{text}");
        assert!(text.contains("FAILED"), "{text}");
    }

    /// A hanging test under a bad fill is information, not a fail.
    #[test]
    fn a_test_past_its_timeout_is_skipped_and_never_failed() {
        let guard = CARGO_ENV.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = scratch("tests-timeout");
        std::fs::create_dir_all(dir.join("tree")).expect("tree");
        let cargo = fake_cargo(&dir, "sleep 30 &\nwait");
        unsafe { std::env::set_var("CHEKOV_CARGO", &cargo) };
        let env = env_for(&dir, Duration::from_secs(5), Duration::from_millis(300));
        let (verdict, _) = super::run_tests(&super::TestRun {
            env: &env,
            krate: "widget",
            tests: &["covers_alpha".to_owned()],
        });
        unsafe { std::env::remove_var("CHEKOV_CARGO") };
        drop(guard);
        let super::TestVerdict::Skipped(reason) = verdict else {
            panic!("expected a skip, got {verdict:?}");
        };
        assert!(reason.starts_with("test timed out after "), "{reason}");
    }
```

`Worktree::detached_for_test(path)` is a `#[cfg(test)] pub(super)` constructor this task adds to `tree.rs` — the test runner needs an `Env` whose worktree is a plain directory, not a registered git worktree, and building one through `add` would need a repository for no reason:

```rust
impl Worktree {
    /// A `Worktree` over a plain directory, for tests that need the PATH and
    /// nothing git does. `removed` is pre-set so `Drop` runs no git command.
    #[cfg(test)]
    pub(super) fn detached_for_test(path: PathBuf) -> Self {
        Self {
            path,
            repo: PathBuf::new(),
            removed: true,
        }
    }
}
```

- [ ] **Step 10: Run them and watch them fail**

Run: `cargo test --locked --lib bench::codebase::exec::tests`
Expected: FAIL — `cannot find type TestRun in module super`.

- [ ] **Step 11: Write the test runner**

Append to the production half of `src/core/bench/codebase/exec.rs`:

```rust
/// Tier 7's inputs (§4 — keeps `run_tests` at one parameter).
pub struct TestRun<'a> {
    pub env: &'a Env,
    pub krate: &'a str,
    pub tests: &'a [String],
}

/// What tier 7 saw. `Skipped` is never a fail: a timeout, an offline
/// registry, or a test module that will not build are all things the fill
/// cannot be blamed for.
#[derive(Debug)]
pub enum TestVerdict {
    Passed,
    Failed(String),
    Skipped(String),
}

/// Every candidate, stopping at the first that does not pass.
///
/// Tier 7 passes only when all of them pass, so there is nothing to learn
/// from running the rest — and the timeout budget is per invocation.
#[must_use]
pub fn run_tests(run: &TestRun) -> (TestVerdict, f64) {
    let mut spent = 0.0;
    for name in run.tests {
        let (verdict, secs) = one_test(run, name);
        spent += secs;
        if !matches!(verdict, TestVerdict::Passed) {
            return (verdict, spent);
        }
    }
    (TestVerdict::Passed, spent)
}

fn one_test(run: &TestRun, name: &str) -> (TestVerdict, f64) {
    let timeout = run.env.timeouts.test;
    let outcome = run_cargo(&CargoRun {
        args: &["test", "-p", run.krate, "--offline", "--", name, "--exact"],
        cwd: &run.env.worktree.path,
        target_dir: &run.env.target_dir,
        timeout,
    });
    let Ok(outcome) = outcome else {
        return (
            TestVerdict::Skipped(format!("cargo test failed to run: {name}")),
            0.0,
        );
    };
    (test_verdict(&outcome, name, timeout), outcome.secs)
}

fn test_verdict(outcome: &CargoOutcome, name: &str, timeout: Duration) -> TestVerdict {
    if outcome.timed_out {
        return TestVerdict::Skipped(format!(
            "test timed out after {} s",
            timeout.as_secs()
        ));
    }
    if let Some(line) = needs_network(&outcome.stderr) {
        return TestVerdict::Skipped(format!("needs network: {line}"));
    }
    if outcome.status == Some(0) {
        return TestVerdict::Passed;
    }
    TestVerdict::Failed(format!("{name}: {}", failure_text(outcome)))
}

/// The most useful line cargo left behind, or a plain statement that it left
/// none — never an empty string standing in for a reason.
fn failure_text(outcome: &CargoOutcome) -> String {
    outcome
        .stdout
        .lines()
        .chain(outcome.stderr.lines())
        .map(str::trim)
        .find(|line| line.contains("FAILED") || line.starts_with("error"))
        .map_or_else(
            || format!("cargo test exited {:?} with no failure line", outcome.status),
            str::to_owned,
        )
}
```

Note `one_test` passes `--offline` and the candidate name through the `--` separator, exactly as the spec's `cargo test -p <crate> --offline -- <t> --exact`.

- [ ] **Step 12: Run them and watch them pass**

Run: `cargo test --locked --lib bench::codebase::exec::tests`
Expected: PASS (24 passed).

- [ ] **Step 13: Run the floor and commit**

Run: `cargo fmt && cargo clippy --locked --all-targets -- -D warnings && cargo test --locked`
Expected: clean; all tests pass.

```bash
git add src/core/bench/codebase/exec.rs src/core/bench/codebase/masker.rs src/core/bench/codebase/tree.rs && git commit -m "$(cat <<'EOF'
feat(codebase): tier 7's half — the enclosing fn, the crate, the covering tests, the runner

exec.rs walks the crate itself rather than reusing tree::rust_sources:
that walk skips tests/ on purpose, because masking an assertion measures
nothing, and tests/ is exactly where a covering test lives.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01YDaTTHJySW2ix3P85BcP6W
EOF
)"
```

---

### Task 5: The worktree's new lifetime, `exec_crossing`, and the run loop

**Files:**
- Modify: `src/core/bench/codebase/mod.rs:130-143` (`Prepared`), `:264-305` (`prepare`), `:307-328` (`into_prepared`, `Sampled`), plus new `PrepareInputs`
- Modify: `src/core/bench/codebase/exec.rs` (`exec_crossing`, `Crossing`, `check_tier`, `test_tier`, the skip constants)
- Modify: `src/core/bench/codebase/run.rs:190-196` (`Recorded`), `:255-300` (`record_codebase_task`), `:327-361` (`run_codebase`), test fixtures `prepared_pair`/`prepared_cross` (`:460`, `:518`)
- Modify: `src/commands/capability.rs:826-838` (`prepare_codebase`)

**Interfaces:**
- Consumes: everything from Tasks 2-4.
- Produces:
  ```rust
  // codebase/mod.rs
  pub struct PrepareInputs<'a> { pub scratch_root: &'a Path, pub tasks: u32, pub allow_exec: bool }
  pub fn prepare(repo: &Path, inputs: &PrepareInputs) -> Result<Prepared, ChekovError>;
  pub struct Prepared { /* … */ pub exec: exec::Exec }

  // codebase/exec.rs
  pub const NO_ENCLOSING_FN: &str = "no enclosing function";
  pub const NO_CRATE: &str = "no crate";
  pub const NO_COVERING_TEST: &str = "no covering test";
  pub const DID_NOT_COMPILE: &str = "did not compile";
  pub fn exec_crossing(env: &Env, task: &CodebaseTask, fill: &str)
      -> Result<store::ExecRow, ChekovError>;

  // codebase/run.rs
  struct Recorded<'a> { outcome: …, symbols: …, arm: &'a Arm, exec: Option<store::ExecRow> }
  ```

- [ ] **Step 1: Write the failing test that `--allow-exec` keeps the worktree**

In `src/core/bench/codebase/mod.rs`'s `mod tests`, add:

```rust
    /// Without the flag the worktree is gone by the time `prepare` returns —
    /// unchanged from slice A. With it, the worktree and the scratch target
    /// directory are both alive, and `finish` takes both away.
    #[test]
    fn the_worktree_survives_prepare_only_when_exec_is_allowed() {
        let (_, off) = prepared_fixture("lifetime-off");
        assert!(matches!(off.exec, crossfile_free_exec_off()), "no flag, no exec");

        let (repo, root) = repo_fixture("lifetime-on");
        let prepared = prepare(
            &repo,
            &super::PrepareInputs {
                scratch_root: &root.join("scratch"),
                tasks: 8,
                allow_exec: true,
            },
        )
        .expect("prepare");
        let env = prepared
            .exec
            .env()
            .expect("a repo with a Cargo.toml and cargo on PATH is ready");
        let (tree, target) = (env.worktree.path.clone(), env.target_dir.clone());
        assert!(tree.is_dir(), "the worktree is kept for the run");
        assert!(target.is_dir(), "the scratch target directory exists");
        prepared.exec.finish().expect("finish");
        assert!(!tree.exists(), "finish removes the worktree");
        assert!(!target.exists(), "and the target directory with it");
    }
```

`repo_fixture(name) -> (PathBuf, PathBuf)` is `prepared_fixture`'s first half, split out in this step so both can use it: it builds and commits the temp repo and returns `(repo, root)`. This test's repo also needs a `Cargo.toml` at its root (the probe demands one) — add `std::fs::write(repo.join("Cargo.toml"), "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n")` to `repo_fixture` before the commit. `crossfile_free_exec_off()` is not a real helper — write the assertion as `matches!(off.exec, crate::core::bench::codebase::exec::Exec::Off)`.

> If `cargo` is not on `PATH` in the test environment, `prepared.exec` is `Unavailable` and this test's second half cannot run. Guard it: `let Some(env) = prepared.exec.env() else { return };` with the comment "no toolchain here — the Unavailable path is covered by `exec::tests`". A test that silently passes for the wrong reason is worse than one that is skipped out loud, so also `eprintln!` the skip.

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test --locked --lib bench::codebase::tests::the_worktree_survives_prepare_only_when_exec_is_allowed`
Expected: FAIL — `cannot find struct PrepareInputs`.

- [ ] **Step 3: Bundle `prepare`'s inputs and hold the exec state**

In `src/core/bench/codebase/mod.rs`, above `prepare` (`:271`):

```rust
/// What `prepare` needs beyond the repository (§4 — three parameters, and
/// the flag would have been a fourth).
pub struct PrepareInputs<'a> {
    pub scratch_root: &'a Path,
    pub tasks: u32,
    /// `--allow-exec`. The single thing that decides whether the worktree
    /// outlives this call.
    pub allow_exec: bool,
}
```

On `Prepared` (`:132`), after `counts`:

```rust
    /// Whether tiers 6-7 run, and — when they do — the worktree, the scratch
    /// target directory and the toolchain they run in. `Exec::Off` is the
    /// slice-A/B1 shape: the worktree was removed before this value existed.
    pub exec: exec::Exec,
```

Rewrite `prepare` (`:271-305`) so the worktree's fate is decided in one place:

```rust
pub fn prepare(repo: &Path, inputs: &PrepareInputs) -> Result<Prepared, ChekovError> {
    tree::assert_clean(repo)?;
    let head = tree::head_sha(repo)?;
    let scratch_tree = inputs
        .scratch_root
        .join(format!("codebase-tree-{}", head12(&head)));
    let worktree = tree::Worktree::add(repo, &scratch_tree)?;
    let sources = tree::rust_sources(&worktree.path);
    let elided = elide_tests(sources.files);
    let index = crossfile::Index::build(&elided.files);
    let mut candidates = all_candidates(&index, &elided);
    let set = sample::sample(
        std::mem::take(&mut candidates.per_file),
        sample::quota(inputs.tasks),
        sample::seed_from_head(&head),
    );
    let symbols = ladder::repo_symbols(&elided.files);
    if set.picked.is_empty() {
        worktree.remove()?;
        return Err(ChekovError::CodebaseNoTasks {
            path: repo.to_path_buf(),
            reason: format!(
                "scanned {} files, {} eligible, 0 candidate spans",
                sources.scanned,
                elided.files.len()
            ),
        });
    }
    let exec = exec_state(worktree, inputs, head12(&head))?;
    Ok(into_prepared(Sampled {
        head,
        set,
        elided,
        candidates,
        symbols,
        oversized: sources.oversized,
        exec,
    }))
}

/// The worktree's fate. Without the flag it is removed here, exactly as in
/// slice A; with it, `exec::prepare_env` keeps it or removes it depending on
/// whether the machine can honour the flag.
fn exec_state(
    worktree: tree::Worktree,
    inputs: &PrepareInputs,
    head12: &str,
) -> Result<exec::Exec, ChekovError> {
    if !inputs.allow_exec {
        worktree.remove()?;
        return Ok(exec::Exec::Off);
    }
    exec::prepare_env(worktree, inputs.scratch_root, head12)
}
```

Add `exec: exec::Exec` to `Sampled` (`:255`) and `exec: s.exec` to the `Prepared` literal in `into_prepared` (`:311`).

> Note the reordering: the `CodebaseNoTasks` check moves **above** the worktree's disposal so the error path removes it explicitly rather than leaving it to `Drop`. The `Drop` is still the backstop for a `?` between `add` and here.

- [ ] **Step 4: Update the two callers and every `Prepared { … }` literal**

In `src/commands/capability.rs`, `prepare_codebase` (`:826`):

```rust
fn prepare_codebase(
    ctx: &Ctx,
    args: &BenchArgs,
) -> Result<Option<crate::core::bench::codebase::Prepared>, ChekovError> {
    let Some(repo) = args.codebase else {
        return Ok(None);
    };
    Ok(Some(crate::core::bench::codebase::prepare(
        repo,
        &crate::core::bench::codebase::PrepareInputs {
            scratch_root: &ctx.config.eval_dir().join(".scratch"),
            tasks: ctx.config.file.bench.codebase_tasks,
            allow_exec: args.allow_exec,
        },
    )?))
}
```

Run `cargo check --locked --all-targets` and add `exec: crate::core::bench::codebase::exec::Exec::Off,` to `prepared_pair` (`run.rs:460`), `prepared_cross` (`run.rs:518`) and any other `Prepared { … }` literal the compiler names.

- [ ] **Step 5: Run the codebase tests and watch them pass**

Run: `cargo test --locked --lib bench::codebase::`
Expected: PASS.

- [ ] **Step 6: Write the failing test for one crossing's two tiers**

In `src/core/bench/codebase/exec.rs`'s `mod tests`, add:

```rust
    /// A crossing whose check reports an error: tier 6 is 0, the message is
    /// stored with its file and line, tier 7 never runs, and the file comes
    /// back.
    #[test]
    fn a_crossing_that_does_not_compile_fails_six_and_skips_seven() {
        let guard = CARGO_ENV.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (dir, env, task, original) = crossing_fixture("no-compile");
        let cargo = fake_cargo(
            &dir,
            "echo '{\"reason\":\"compiler-message\",\"message\":{\"level\":\"error\",\
             \"message\":\"mismatched types\",\"spans\":[{\"file_name\":\"src/a.rs\",\
             \"line_start\":2,\"is_primary\":true}]}}'\nexit 101",
        );
        unsafe { std::env::set_var("CHEKOV_CARGO", &cargo) };
        let row = super::exec_crossing(&env, &task, "let a = \"two\";").expect("the crossing runs");
        unsafe { std::env::remove_var("CHEKOV_CARGO") };
        drop(guard);
        assert_eq!(row.compile, crate::core::bench::store::ExecScore::Value(0.0));
        assert_eq!(
            row.compile_error.as_deref(),
            Some("src/a.rs:2: mismatched types")
        );
        assert_eq!(
            row.test,
            crate::core::bench::store::ExecScore::Skipped(super::DID_NOT_COMPILE.to_owned())
        );
        assert!(row.tests.is_empty());
        assert_eq!(
            std::fs::read_to_string(env.worktree.path.join("src/a.rs")).expect("read"),
            original,
            "the revert restored the file"
        );
        env.finish().expect("cleanup");
    }

    /// A clean check with no test naming the symbol: tier 6 passes, tier 7 is
    /// a counted reason and never a zero.
    #[test]
    fn a_crossing_with_no_covering_test_passes_six_and_says_why_seven_did_not_run() {
        let guard = CARGO_ENV.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (dir, env, task, _) = crossing_fixture("no-covering");
        let cargo = fake_cargo(&dir, "exit 0");
        unsafe { std::env::set_var("CHEKOV_CARGO", &cargo) };
        let row = super::exec_crossing(&env, &task, "let a = 1;").expect("the crossing runs");
        unsafe { std::env::remove_var("CHEKOV_CARGO") };
        drop(guard);
        assert_eq!(row.compile, crate::core::bench::store::ExecScore::Value(1.0));
        assert_eq!(
            row.test,
            crate::core::bench::store::ExecScore::Skipped(super::NO_COVERING_TEST.to_owned())
        );
        assert!(row.check_secs >= 0.0);
        env.finish().expect("cleanup");
    }

    /// The offline registry is a skip with cargo's own words, and never a
    /// compile failure the model is blamed for.
    #[test]
    fn a_check_that_wants_the_registry_is_skipped_with_cargos_words() {
        let guard = CARGO_ENV.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (dir, env, task, _) = crossing_fixture("offline");
        let cargo = fake_cargo(
            &dir,
            "echo 'error: no matching package named `serde` found' >&2\nexit 101",
        );
        unsafe { std::env::set_var("CHEKOV_CARGO", &cargo) };
        let row = super::exec_crossing(&env, &task, "let a = 1;").expect("the crossing runs");
        unsafe { std::env::remove_var("CHEKOV_CARGO") };
        drop(guard);
        let crate::core::bench::store::ExecScore::Skipped(reason) = row.compile else {
            panic!("expected a skip, got {:?}", row.compile);
        };
        assert!(reason.starts_with("needs network: "), "{reason}");
        assert!(reason.contains("no matching package named"), "{reason}");
        env.finish().expect("cleanup");
    }

    #[test]
    fn a_check_past_its_timeout_is_skipped_and_the_file_is_still_restored() {
        let guard = CARGO_ENV.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (dir, env, task, original) = crossing_fixture_with("timeout-check", Duration::from_millis(300));
        let cargo = fake_cargo(&dir, "sleep 30 &\nwait");
        unsafe { std::env::set_var("CHEKOV_CARGO", &cargo) };
        let row = super::exec_crossing(&env, &task, "let a = 1;").expect("the crossing runs");
        unsafe { std::env::remove_var("CHEKOV_CARGO") };
        drop(guard);
        let crate::core::bench::store::ExecScore::Skipped(reason) = row.compile else {
            panic!("expected a skip, got {:?}", row.compile);
        };
        assert!(reason.starts_with("check timed out after "), "{reason}");
        assert_eq!(
            std::fs::read_to_string(env.worktree.path.join("src/a.rs")).expect("read"),
            original
        );
        env.finish().expect("cleanup");
    }
```

The two fixture helpers build a committed one-crate repo, add a real `Worktree`, and hand back a `CodebaseTask` whose `byte_range` lands on `let a = 1;` in `src/a.rs`:

```rust
    fn crossing_fixture(name: &str) -> (PathBuf, super::Env, super::super::CodebaseTask, String) {
        crossing_fixture_with(name, Duration::from_secs(30))
    }

    fn crossing_fixture_with(
        name: &str,
        check: Duration,
    ) -> (PathBuf, super::Env, super::super::CodebaseTask, String) {
        use super::super::{CodebaseTask, Excluded, TaskTier};
        let dir = scratch(name);
        let repo = dir.join("repo");
        std::fs::create_dir_all(repo.join("src")).expect("src");
        std::fs::write(repo.join("Cargo.toml"), "[package]\nname = \"widget\"\nversion = \"0.1.0\"\n")
            .expect("manifest");
        std::fs::write(repo.join("src/a.rs"), "fn f() {\nlet a = 1;\n}\n").expect("a.rs");
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "t@t"],
            vec!["config", "user.name", "t"],
            vec!["add", "-A"],
            vec!["commit", "-qm", "fixture"],
        ] {
            super::super::tree::git(&repo, &args, "fixture").expect("git");
        }
        let worktree =
            super::super::tree::Worktree::add(&repo, &dir.join("tree")).expect("worktree");
        let original =
            std::fs::read_to_string(worktree.path.join("src/a.rs")).expect("the original");
        let env = super::Env {
            worktree,
            target_dir: dir.join("target"),
            cargo_version: "cargo 1.95.0".to_owned(),
            timeouts: Timeouts { check, test: Duration::from_secs(30) },
        };
        let task = CodebaseTask {
            id: "in_file-abc123-L2".into(),
            tier: TaskTier::InFile,
            file: "src/a.rs".into(),
            line: 2,
            byte_range: 9..19,
            gold: "let a = 1;".into(),
            prefix: "fn f() {\n".into(),
            suffix: "\n}\n".into(),
            excluded: Excluded {
                doc_comment: 0,
                cross_file: "n/a: same-file".into(),
                cfg_test_lines: 0,
                cross_file_withheld: 0,
            },
            name: None,
            also_first_uses: Vec::new(),
            extra: None,
            extra_text: String::new(),
        };
        (dir, env, task, original)
    }
```

- [ ] **Step 7: Run them and watch them fail**

Run: `cargo test --locked --lib bench::codebase::exec::tests`
Expected: FAIL — `cannot find function exec_crossing in module super`.

- [ ] **Step 8: Write `exec_crossing`**

Append to the production half of `src/core/bench/codebase/exec.rs`:

```rust
use crate::core::bench::store::{ExecRow, ExecScore};

use super::CodebaseTask;

/// Tier 7's reason when the span sits outside every function body.
pub const NO_ENCLOSING_FN: &str = "no enclosing function";
/// Tier 7's reason when no `Cargo.toml` above the file names a package.
pub const NO_CRATE: &str = "no crate";
/// Tier 7's reason when nothing in the crate tests the symbol.
pub const NO_COVERING_TEST: &str = "no covering test";
/// Tier 7's reason when tier 6 did not pass (spec §4).
pub const DID_NOT_COMPILE: &str = "did not compile";

/// One crossing's tiers 6 and 7: splice, check, tests, revert.
///
/// The revert runs whatever the tiers decided, and its failure is the run's
/// abort. Nothing here returns an `Err` for a cargo outcome — a timeout, an
/// offline registry, an unbuildable test module are all skips with reasons,
/// because none of them is the model's answer being wrong.
pub fn exec_crossing(
    env: &Env,
    task: &CodebaseTask,
    fill: &str,
) -> Result<ExecRow, ChekovError> {
    let path = env.worktree.path.join(&task.file);
    let original = std::fs::read_to_string(&path)
        .map_err(|e| ChekovError::io(format!("reading {}", path.display()), e))?;
    apply(
        &Splice {
            path: &path,
            original: &original,
            span: task.byte_range.clone(),
        },
        fill,
    )?;
    let row = tiers(env, task, &original);
    revert(env, &task.file, &original)?;
    Ok(row)
}

/// Tier 6, and tier 7 only if tier 6 passed.
fn tiers(env: &Env, task: &CodebaseTask, original: &str) -> ExecRow {
    let (compile, compile_error, check_secs) = check_tier(env);
    let mut row = ExecRow {
        compile,
        compile_error,
        tests: Vec::new(),
        test: ExecScore::Skipped(DID_NOT_COMPILE.to_owned()),
        test_failure: None,
        check_secs,
        test_secs: 0.0,
    };
    if row.compile != ExecScore::Value(1.0) {
        return row;
    }
    let seven = test_tier(env, task, original);
    row.tests = seven.tests;
    row.test = seven.score;
    row.test_failure = seven.failure;
    row.test_secs = seven.secs;
    row
}

/// `cargo check --message-format=json --offline`, judged by its diagnostics.
///
/// The exit status is not the verdict: cargo exits non-zero for errors it
/// also reports, and the stream is the auditable record. A workspace-wide
/// error counts — a fill that breaks a caller in another file is exactly what
/// the cross-file tier is for.
fn check_tier(env: &Env) -> (ExecScore, Option<String>, f64) {
    let outcome = run_cargo(&CargoRun {
        args: &["check", "--message-format=json", "--offline"],
        cwd: &env.worktree.path,
        target_dir: &env.target_dir,
        timeout: env.timeouts.check,
    });
    let Ok(outcome) = outcome else {
        return (
            ExecScore::Skipped("cargo check failed to run".to_owned()),
            None,
            0.0,
        );
    };
    let (score, error) = check_score(&outcome, env.timeouts.check);
    (score, error, outcome.secs)
}

fn check_score(outcome: &CargoOutcome, timeout: Duration) -> (ExecScore, Option<String>) {
    if outcome.timed_out {
        return (
            ExecScore::Skipped(format!("check timed out after {} s", timeout.as_secs())),
            None,
        );
    }
    if let Some(line) = needs_network(&outcome.stderr) {
        return (ExecScore::Skipped(format!("needs network: {line}")), None);
    }
    match first_error(&outcome.stdout) {
        Some(error) => (ExecScore::Value(0.0), Some(error)),
        None => (ExecScore::Value(1.0), None),
    }
}

/// What tier 7 produced.
struct Seven {
    tests: Vec<String>,
    score: ExecScore,
    failure: Option<String>,
    secs: f64,
}

impl Seven {
    fn skipped(reason: &str) -> Self {
        Self {
            tests: Vec::new(),
            score: ExecScore::Skipped(reason.to_owned()),
            failure: None,
            secs: 0.0,
        }
    }
}

/// The symbol, the crate, the candidates, the run.
fn test_tier(env: &Env, task: &CodebaseTask, original: &str) -> Seven {
    let Some(symbols) = tier_seven_symbols(task, original) else {
        return Seven::skipped(NO_ENCLOSING_FN);
    };
    let Some(krate) = crate_of(&env.worktree.path, &task.file) else {
        return Seven::skipped(NO_CRATE);
    };
    let tests = covering_tests(&krate.root, &symbols);
    if tests.is_empty() {
        return Seven::skipped(NO_COVERING_TEST);
    }
    let (verdict, secs) = run_tests(&TestRun {
        env,
        krate: &krate.name,
        tests: &tests,
    });
    seven_from(verdict, tests, secs)
}

/// The enclosing function, plus the symbol a cross-file crossing is keyed on.
fn tier_seven_symbols(task: &CodebaseTask, original: &str) -> Option<Vec<String>> {
    let enclosing = super::masker::enclosing_fn(original, task.byte_range.start)?;
    let mut symbols = vec![enclosing];
    symbols.extend(task.name.clone());
    Some(symbols)
}

fn seven_from(verdict: TestVerdict, tests: Vec<String>, secs: f64) -> Seven {
    match verdict {
        TestVerdict::Passed => Seven {
            tests,
            score: ExecScore::Value(1.0),
            failure: None,
            secs,
        },
        TestVerdict::Failed(text) => Seven {
            tests,
            score: ExecScore::Value(0.0),
            failure: Some(text),
            secs,
        },
        TestVerdict::Skipped(reason) => Seven {
            tests,
            score: ExecScore::Skipped(reason),
            failure: None,
            secs,
        },
    }
}
```

- [ ] **Step 9: Run them and watch them pass**

Run: `cargo test --locked --lib bench::codebase::exec::tests`
Expected: PASS (28 passed).

- [ ] **Step 10: Write the failing tests for the run loop**

In `src/core/bench/codebase/run.rs`'s `mod tests`, add a helper that turns a prepared set's `exec` on with a fake cargo, and three tests:

```rust
    /// A prepared set whose exec half is a real worktree over a one-crate
    /// repo, and a fake cargo that answers `script`.
    fn with_exec(name: &str, script: &str) -> (Prepared, std::path::PathBuf) {
        use crate::core::bench::codebase::exec;
        let dir = std::env::temp_dir().join("chekov-test-run-exec").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        let repo = dir.join("repo");
        std::fs::create_dir_all(repo.join("src")).expect("src");
        std::fs::write(
            repo.join("Cargo.toml"),
            "[package]\nname = \"widget\"\nversion = \"0.1.0\"\n",
        )
        .expect("manifest");
        std::fs::write(repo.join("src/a.rs"), "fn f() {\nlet a = 1;\n}\n").expect("a.rs");
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "t@t"],
            vec!["config", "user.name", "t"],
            vec!["add", "-A"],
            vec!["commit", "-qm", "fixture"],
        ] {
            crate::core::bench::codebase::tree::git(&repo, &args, "fixture").expect("git");
        }
        let worktree = crate::core::bench::codebase::tree::Worktree::add(&repo, &dir.join("tree"))
            .expect("worktree");
        let mut prepared = prepared_pair();
        prepared.tasks.truncate(1);
        prepared.tasks[0].file = "src/a.rs".into();
        prepared.tasks[0].byte_range = 9..19;
        prepared.counts.in_file = 1;
        prepared.exec = exec::Exec::Ready(exec::Env {
            worktree,
            target_dir: dir.join("target"),
            cargo_version: "cargo 1.95.0 (fake)".to_owned(),
            timeouts: exec::Timeouts::DEFAULT,
        });
        let cargo = fake_cargo(&dir, script);
        (prepared, cargo)
    }

    /// The same shell-script cargo the exec tests use.
    fn fake_cargo(dir: &std::path::Path, body: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(dir).expect("dir");
        let path = dir.join("fake-cargo");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        path
    }

    static CARGO_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn an_answered_crossing_carries_its_exec_verdict_onto_the_row() {
        let guard = CARGO_ENV.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (prepared, cargo) = with_exec("answered", "exit 0");
        unsafe { std::env::set_var("CHEKOV_CARGO", &cargo) };
        let (rows, _, _) = drive("exec-answered", &prepared, (vec![Ok(infill_200())], vec![]));
        unsafe { std::env::remove_var("CHEKOV_CARGO") };
        drop(guard);
        let exec = rows[0]
            .codebase
            .as_ref()
            .and_then(|c| c.exec.clone())
            .expect("the crossing recorded its exec half");
        assert_eq!(
            exec.compile,
            crate::core::bench::store::ExecScore::Value(1.0)
        );
        prepared.exec.finish().expect("cleanup");
    }

    /// A crossing nobody answered has no fill to splice, so it has no exec
    /// half at all — a `Skipped` there would claim a question was asked.
    #[test]
    fn an_unanswered_crossing_has_no_exec_half() {
        let guard = CARGO_ENV.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (prepared, cargo) = with_exec("unanswered", "exit 0");
        unsafe { std::env::set_var("CHEKOV_CARGO", &cargo) };
        let (rows, _, _) = drive(
            "exec-unanswered",
            &prepared,
            (vec![refused("the server is out of context")], vec![]),
        );
        unsafe { std::env::remove_var("CHEKOV_CARGO") };
        drop(guard);
        assert!(rows[0].codebase.as_ref().is_some_and(|c| c.exec.is_none()));
        prepared.exec.finish().expect("cleanup");
    }

    /// No toolchain: every crossing records the one reason, and no cargo is
    /// ever spawned.
    #[test]
    fn an_unavailable_toolchain_skips_every_crossing_with_one_reason() {
        let mut prepared = prepared_pair();
        prepared.exec = crate::core::bench::codebase::exec::Exec::Unavailable(
            "no Rust toolchain: cargo is not runnable".to_owned(),
        );
        let (rows, _, _) = drive(
            "exec-unavailable",
            &prepared,
            (vec![Ok(infill_200()), Ok(infill_200())], vec![]),
        );
        for row in &rows {
            let exec = row
                .codebase
                .as_ref()
                .and_then(|c| c.exec.clone())
                .expect("the reason is recorded per crossing");
            assert_eq!(
                exec.compile,
                crate::core::bench::store::ExecScore::Skipped(
                    "no Rust toolchain: cargo is not runnable".to_owned()
                )
            );
            assert_eq!(exec.compile, exec.test);
        }
    }
```

- [ ] **Step 11: Run them and watch them fail**

Run: `cargo test --locked --lib bench::codebase::run::tests`
Expected: FAIL — `no field exec on type CodebaseRow`… no: FAIL at `c.exec` being always `None`, because nothing sets it yet. Read the failure and confirm it is the assertion, not a compile error.

- [ ] **Step 12: Hook the exec step into the loop**

In `src/core/bench/codebase/run.rs`, add to `Recorded` (`:192`):

```rust
    /// Tiers 6-7, when the run was allowed to build. `None` when it was not,
    /// and on a crossing nobody answered — there was no fill to splice, and a
    /// skip there would claim a question that was never asked.
    exec: Option<store::ExecRow>,
```

Destructure it in `record_codebase_task` (`:264`) and add `exec: recorded.exec` — restructured so the destructuring stays readable:

```rust
    let Recorded {
        outcome,
        symbols,
        arm,
        exec,
    } = recorded;
```

and in the `CodebaseRow` literal, after `n_predict`:

```rust
            exec,
```

Replace `run_codebase` (`:333`) and add the crossing helper and the timing:

```rust
/// What the exec tiers cost so far, so the run can say what the rest will.
struct ExecTiming {
    cold: Option<f64>,
    later: Vec<f64>,
    announced: bool,
}

impl ExecTiming {
    const fn new() -> Self {
        Self {
            cold: None,
            later: Vec::new(),
            announced: false,
        }
    }

    /// One check's wall clock, and — the second time round — the line the
    /// spec's live estimate asks for. Printed once: the first check pays for
    /// the whole target directory, and every check after it is incremental,
    /// so one number cannot describe both.
    fn record(&mut self, secs: f64) {
        let Some(cold) = self.cold else {
            self.cold = Some(secs);
            return;
        };
        self.later.push(secs);
        if !self.announced {
            self.announced = true;
            eprintln!(
                "chekov bench: exec cold check {cold:.0} s, ~{secs:.0} s per crossing thereafter"
            );
        }
    }
}

/// Tiers 6-7 for one answered crossing, or `None` when they did not apply.
fn exec_row(
    prepared: &super::Prepared,
    task: &CodebaseTask,
    parts: &ExecInput,
) -> Result<Option<store::ExecRow>, ChekovError> {
    let Some(prediction) = parts.prediction else {
        return Ok(None);
    };
    match &prepared.exec {
        super::exec::Exec::Off => Ok(None),
        super::exec::Exec::Unavailable(reason) => Ok(Some(store::ExecRow::skipped(reason))),
        super::exec::Exec::Ready(env) => {
            let fill = ladder::trimmed_to_gold(&task.gold, prediction);
            let row = super::exec::exec_crossing(env, task, &fill)?;
            parts.timing.borrow_mut().record(row.check_secs);
            Ok(Some(row))
        }
    }
}

/// What `exec_row` needs beyond the prepared set and the task (§4).
struct ExecInput<'a> {
    /// The raw prediction, or `None` when the crossing was never answered.
    prediction: Option<&'a str>,
    timing: &'a std::cell::RefCell<ExecTiming>,
}

pub fn run_codebase(
    sink: &mut Sink,
    wire: &runner::ProbeWire,
    prepared: &super::Prepared,
) -> Result<(), ChekovError> {
    let mut unsupported: Option<String> = None;
    let timing = std::cell::RefCell::new(ExecTiming::new());
    for task in &prepared.tasks {
        for arm in arms(task) {
            if sink.is_done(&TaskKey::buffered("codebase", &arm.id)) {
                continue;
            }
            let outcome = infill_or_latch(
                wire,
                &Crossing {
                    task,
                    with_extra: arm.with_extra,
                },
                &mut unsupported,
            );
            let prediction = outcome.as_ref().ok().map(|a| a.anthropic_body.clone());
            let exec = exec_row(
                prepared,
                task,
                &ExecInput {
                    prediction: prediction.as_deref(),
                    timing: &timing,
                },
            )?;
            record_codebase_task(
                sink,
                task,
                Recorded {
                    outcome,
                    symbols: &prepared.symbols,
                    arm: &arm,
                    exec,
                },
            )?;
        }
    }
    Ok(())
}
```

`run_codebase` is now 30 lines; if it drifts past 40, lift the body of the inner `for` into `fn one_crossing(sink, prepared, ctx: &CrossingCtx) -> Result<(), ChekovError>`.

- [ ] **Step 13: Run them and watch them pass**

Run: `cargo test --locked --lib bench::codebase::run::tests`
Expected: PASS (every existing run test plus the three new ones).

- [ ] **Step 14: Run the floor and commit**

Run: `cargo fmt && cargo clippy --locked --all-targets -- -D warnings && cargo test --locked`
Expected: clean; all tests pass.

```bash
git add src/core/bench/codebase/ src/commands/capability.rs && git commit -m "$(cat <<'EOF'
feat(codebase): the worktree outlives prepare under --allow-exec, and every crossing gets its two exec tiers

The exec state is one three-value enum on Prepared, so the worktree's
fate has exactly one answer per outcome: Off and Unavailable remove it in
prepare as slice A did, Ready keeps it and finish() takes it away with
the scratch target dir. A crossing nobody answered has no exec half at
all — a Skipped there would claim a question that was never asked.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01YDaTTHJySW2ix3P85BcP6W
EOF
)"
```

---

### Task 6: `store.rs` — the two cells, the two lift columns, the trailer, the header clause

**Files:**
- Modify: `src/core/bench/store.rs:698-722` (`render_codebase`), `:724-742` (`codebase_header`), `:835-853` (`scores_line`), `:860-880` (`lift_line`), plus new `exec_cells`, `exec_mean`, `exec_delta_cells`, `exec_trailer`, `Header`
- Modify: `src/core/bench/store.rs` `mod tests` (fixtures + exact-string tests)

**Interfaces:**
- Consumes: `ExecScore`, `ExecRow`, `CodebaseRow.exec` (Task 1); `stamp::Stamp.{allow_exec, cargo_version}` (Task 1).
- Produces (all private to `store.rs`):
  ```rust
  struct Header<'a> { kept: &'a [&'a TaskRow], excluded: usize, stamp: &'a Stamp }
  fn codebase_header(header: &Header) -> String;
  fn exec_cells(group: &[&CodebaseRow]) -> Vec<String>;
  fn exec_mean<'a>(scores: impl Iterator<Item = &'a ExecScore>) -> Option<(f64, usize)>;
  fn exec_delta_cells(pairs: &[(&CodebaseRow, &CodebaseRow)]) -> Vec<String>;
  fn exec_trailer(rows: &[&TaskRow], stamp: &Stamp) -> String;
  ```

**The exact lines this task produces.** `scores_line` joins cells with three spaces and `lift_line` with two; the exec cells are pushed onto those same vectors, so:

```
             in_file                 exact 0.50   edit_sim 0.74   ident_f1 0.87   parse 0.92   symbols 0.95 (scored at run time)   compile 0.83 (n=12)   test 1.00 (n=3 of 12 had a covering test)   (n=12)
             context lift            exact +0.33  edit_sim +0.30  symbols +0.08  compile +0.33  test n/a   (6 files sent, 41.2 KiB, 0 truncated; 0 withheld)
             tiers 6-7: cold check 84 s, then 6 s median per crossing; 3 skipped (2 check timed out after 120 s, 1 needs network)
```

and, per the spec's other three trailer cases:

```
             tiers 6-7 skipped: --allow-exec not given
             tiers 6-7 skipped: no Rust toolchain: cargo is not runnable
             tiers 6-7: cold check 84 s, then 6 s median per crossing
```

- [ ] **Step 1: Write the failing tests for the cells and the trailer**

In `src/core/bench/store.rs`'s `mod tests`, add a fixture and four tests:

```rust
    /// A codebase task whose exec half is what the caller says.
    fn exec_task(id: &str, compile: super::ExecScore, tests: &[&str]) -> Task {
        let mut task = codebase_task(CodebaseFixture {
            id,
            tier: TaskTier::InFile,
            gold: "let a = 1;",
            prediction: "let a = 1;",
        });
        task.task_id = id.into();
        let passed = compile == super::ExecScore::Value(1.0);
        if let Some(row) = task.codebase.as_mut() {
            row.exec = Some(super::ExecRow {
                compile,
                compile_error: None,
                tests: tests.iter().map(|t| (*t).to_owned()).collect(),
                test: if !passed {
                    super::ExecScore::Skipped("did not compile".into())
                } else if tests.is_empty() {
                    super::ExecScore::Skipped("no covering test".into())
                } else {
                    super::ExecScore::Value(1.0)
                },
                test_failure: None,
                check_secs: 6.0,
                test_secs: 0.0,
            });
        }
        task
    }

    /// A head whose stamp says the run was allowed to build.
    fn exec_head() -> RunHead {
        let mut head = head();
        head.stamp.allow_exec = true;
        head.stamp.cargo_version = Some("cargo 1.95.0 (deadbeef 2026-01-01)".into());
        head.stamp.exec_target = "scratch".into();
        head
    }

    #[test]
    fn a_tier_line_carries_the_two_exec_cells_after_symbols() {
        let eval = scratch("codebase-exec-cells");
        let mut writer = RunWriter::create(&eval, "r30-model", &exec_head()).expect("create");
        writer
            .append(exec_task(
                "in_file-abc123-L7",
                super::ExecScore::Value(1.0),
                &["covers_alpha"],
            ))
            .expect("append");
        writer
            .append(exec_task(
                "in_file-abc123-L9",
                super::ExecScore::Value(0.0),
                &[],
            ))
            .expect("append");
        let block = super::render_codebase(&RunLog::load(writer.dir()).expect("load"));
        assert!(
            block.contains("compile 0.50 (n=2)   test 1.00 (n=1 of 2 had a covering test)"),
            "{block}"
        );
    }

    /// Every crossing skipped: the cell says `n/a`, never `0.00`.
    #[test]
    fn a_tier_line_with_no_verdict_says_n_a_and_never_zero() {
        let eval = scratch("codebase-exec-na");
        let mut writer = RunWriter::create(&eval, "r31-model", &exec_head()).expect("create");
        writer
            .append(exec_task(
                "in_file-abc123-L7",
                super::ExecScore::Skipped("needs network: no matching package".into()),
                &[],
            ))
            .expect("append");
        let block = super::render_codebase(&RunLog::load(writer.dir()).expect("load"));
        assert!(
            block.contains("compile n/a   test n/a (0 of 1 had a covering test)"),
            "{block}"
        );
        assert!(!block.contains("compile 0.00"), "a skip is not a zero: {block}");
    }

    #[test]
    fn the_header_names_the_toolchain_and_the_trailer_the_timing_and_the_skips() {
        let eval = scratch("codebase-exec-trailer");
        let mut writer = RunWriter::create(&eval, "r32-model", &exec_head()).expect("create");
        let mut cold = exec_task("in_file-abc123-L7", super::ExecScore::Value(1.0), &["t"]);
        if let Some(row) = cold.codebase.as_mut() {
            if let Some(exec) = row.exec.as_mut() {
                exec.check_secs = 84.0;
            }
        }
        writer.append(cold).expect("append");
        for (i, secs) in [6.0_f64, 6.0, 7.0].into_iter().enumerate() {
            let mut task = exec_task(
                &format!("in_file-abc123-L{}", 10 + i),
                super::ExecScore::Value(1.0),
                &["t"],
            );
            if let Some(row) = task.codebase.as_mut() {
                if let Some(exec) = row.exec.as_mut() {
                    exec.check_secs = secs;
                }
            }
            writer.append(task).expect("append");
        }
        writer
            .append(exec_task(
                "in_file-abc123-L20",
                super::ExecScore::Skipped("check timed out after 120 s".into()),
                &[],
            ))
            .expect("append");
        let block = super::render_codebase(&RunLog::load(writer.dir()).expect("load"));
        assert!(
            block.contains("; exec: cargo 1.95.0 (deadbeef 2026-01-01), offline, scratch target"),
            "{block}"
        );
        assert!(
            block.contains(
                "             tiers 6-7: cold check 84 s, then 6 s median per crossing; \
                 1 skipped (1 check timed out after 120 s)\n"
            ),
            "{block}"
        );
    }

    /// The two runs that never built anything each say so in their own words.
    #[test]
    fn a_run_without_the_flag_and_a_run_without_a_toolchain_say_different_things() {
        let eval = scratch("codebase-exec-off");
        let mut writer = RunWriter::create(&eval, "r33-model", &head()).expect("create");
        writer
            .append(codebase_task(CodebaseFixture {
                id: "in_file-abc123-L7",
                tier: TaskTier::InFile,
                gold: "let a = 1;",
                prediction: "let a = 1;",
            }))
            .expect("append");
        let block = super::render_codebase(&RunLog::load(writer.dir()).expect("load"));
        assert!(
            block.contains("             tiers 6-7 skipped: --allow-exec not given\n"),
            "{block}"
        );
        assert!(!block.contains("compile"), "no cells without the flag: {block}");

        let eval = scratch("codebase-exec-no-toolchain");
        let mut writer = RunWriter::create(&eval, "r34-model", &exec_head()).expect("create");
        writer
            .append(exec_task(
                "in_file-abc123-L7",
                super::ExecScore::Skipped("no Rust toolchain: cargo is not runnable".into()),
                &[],
            ))
            .expect("append");
        let block = super::render_codebase(&RunLog::load(writer.dir()).expect("load"));
        assert!(
            block.contains(
                "             tiers 6-7 skipped: no Rust toolchain: cargo is not runnable\n"
            ),
            "{block}"
        );
    }
```

> The clause is `; exec: {cargo_version}, offline, scratch target` and `cargo --version` already begins with the word `cargo`, so there is no second `cargo` to add: the rendered line reads `; exec: cargo 1.95.0 (deadbeef 2026-01-01), offline, scratch target`.

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test --locked --lib bench::store::tests`
Expected: FAIL — the four new tests, on the missing substrings.

- [ ] **Step 3: Write the cells, the header clause and the trailer**

In `src/core/bench/store.rs`, replace `render_codebase`'s body (`:699-722`) so the header and the trailer both see the stamp:

```rust
pub fn render_codebase(log: &RunLog) -> String {
    let rows: Vec<&TaskRow> = rows_of(log, "codebase")
        .filter(|r| r.codebase.is_some())
        .collect();
    if rows.is_empty() {
        return String::new();
    }
    let (kept, excluded) = measured(&rows);
    if kept.is_empty() {
        return codebase_na_line(&rows);
    }
    let mut out = codebase_header(&Header {
        kept: &kept,
        excluded,
        stamp: &log.head.stamp,
    });
    out.push_str(&scores_line(
        "in_file",
        &group(&kept, TaskTier::InFile, None),
    ));
    out.push_str(&scores_line(
        "function_body",
        &group(&kept, TaskTier::FunctionBody, None),
    ));
    out.push_str(&cross_lines(&rows, &kept));
    out.push_str(&exec_trailer(&kept, &log.head.stamp));
    out
}

/// What the header line reads (§4 — three parameters).
struct Header<'a> {
    kept: &'a [&'a TaskRow],
    excluded: usize,
    stamp: &'a crate::core::bench::stamp::Stamp,
}
```

Rewrite `codebase_header` (`:724`) to take it and append the exec clause:

```rust
fn codebase_header(header: &Header) -> String {
    let kept = header.kept;
    let counts = crate::core::bench::codebase::Counts {
        in_file: tier_tasks(kept, TaskTier::InFile),
        function_body: tier_tasks(kept, TaskTier::FunctionBody),
        cross_file_first: tier_tasks(kept, TaskTier::CrossFileFirst),
    };
    format!(
        "codebase     {} tasks, {} crossings, from {} files ({}) — {}; context: same-file, \
         plus the defining file for cross_file_first (engine window ≤ n_batch; extra from \
         ctx); tiers 1-4 score the first gold_lines lines of each fill{}{}{}\n",
        distinct_tasks(kept),
        kept.len(),
        distinct_files(kept),
        crate::core::bench::codebase::tier_counts_clause(counts),
        crate::core::bench::codebase::MASK_LABEL,
        elided_note(kept),
        exec_clause(header.stamp),
        excluded_note(header.excluded),
    )
}

/// `; exec: cargo 1.95.0 (…), offline, scratch target` — what the exec tiers
/// ran under, once per run, and nothing at all when they did not run.
fn exec_clause(stamp: &crate::core::bench::stamp::Stamp) -> String {
    match (stamp.allow_exec, stamp.cargo_version.as_deref()) {
        (true, Some(version)) => format!("; exec: {version}, offline, scratch target"),
        _ => String::new(),
    }
}
```

Append the exec cells inside `scores_line` (`:837`), after `cells.push(symbols_cell(group));`:

```rust
    cells.extend(exec_cells(group));
```

and add, below `symbols_cell` (`:972`):

```rust
/// `compile 0.83 (n=12)` and `test 1.00 (n=3 of 12 had a covering test)`, or
/// nothing at all when this run never ran the exec tiers.
///
/// `compile`'s `n` counts crossings with a VERDICT: a skip is excluded from
/// the mean and counted in the trailer by reason, because averaging a skip in
/// as a zero would score the model down for a question the machine could not
/// ask. `test`'s parenthetical always says how many crossings had a covering
/// test at all — the number that makes the mean readable.
fn exec_cells(group: &[&CodebaseRow]) -> Vec<String> {
    let execs: Vec<&ExecRow> = group.iter().filter_map(|c| c.exec.as_ref()).collect();
    if execs.is_empty() {
        return Vec::new();
    }
    let covered = execs.iter().filter(|e| !e.tests.is_empty()).count();
    let total = execs.len();
    let compile = match exec_mean(execs.iter().map(|e| &e.compile)) {
        Some((mean, n)) => format!("compile {mean:.2} (n={n})"),
        None => "compile n/a".to_owned(),
    };
    let test = match exec_mean(execs.iter().map(|e| &e.test)) {
        Some((mean, n)) => format!("test {mean:.2} (n={n} of {total} had a covering test)"),
        None => format!("test n/a ({covered} of {total} had a covering test)"),
    };
    vec![compile, test]
}

/// The mean of the scored values and how many there were — `None` when every
/// one of them was skipped.
fn exec_mean<'a>(scores: impl Iterator<Item = &'a ExecScore>) -> Option<(f64, usize)> {
    let values: Vec<f64> = scores
        .filter_map(|s| match s {
            ExecScore::Value(v) => Some(*v),
            ExecScore::Skipped(_) => None,
        })
        .collect();
    if values.is_empty() {
        return None;
    }
    Some((values.iter().sum::<f64>() / as_f64(values.len()), values.len()))
}
```

> `test`'s `n=` is the count of crossings with a tier-7 **verdict**, and `covered` the count with a candidate. They coincide except when a covering test timed out, which is exactly when the difference is worth seeing.

- [ ] **Step 4: Write the trailer**

Below `exec_mean`:

```rust
/// The block's last line: what the exec tiers cost, or why there are none.
///
/// Three shapes, and the rows decide which: no exec half anywhere is a run
/// that was never given the flag; every crossing skipped for one reason is
/// that reason, said once; anything else is the timing plus the skips
/// counted by reason.
fn exec_trailer(rows: &[&TaskRow], stamp: &crate::core::bench::stamp::Stamp) -> String {
    let execs: Vec<&ExecRow> = rows
        .iter()
        .filter_map(|r| r.codebase.as_ref()?.exec.as_ref())
        .collect();
    if execs.is_empty() || !stamp.allow_exec {
        return "             tiers 6-7 skipped: --allow-exec not given\n".to_owned();
    }
    let checks: Vec<f64> = execs
        .iter()
        .filter(|e| matches!(e.compile, ExecScore::Value(_)))
        .map(|e| e.check_secs)
        .collect();
    let skips = skip_tally(&execs);
    if checks.is_empty() {
        return format!(
            "             tiers 6-7 skipped: {}\n",
            one_reason(&skips).unwrap_or_else(|| "no crossing produced a verdict".to_owned())
        );
    }
    format!(
        "             tiers 6-7: cold check {:.0} s, then {:.0} s median per crossing{}\n",
        checks[0],
        median(&checks[1..]).unwrap_or(checks[0]),
        skip_note(&skips),
    )
}

/// Every skip reason with its count, most frequent first and ties by reason —
/// a stable order, so the line is testable.
fn skip_tally(execs: &[&ExecRow]) -> Vec<(String, usize)> {
    let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for exec in execs {
        if let ExecScore::Skipped(reason) = &exec.compile {
            *counts.entry(reason.as_str()).or_default() += 1;
        }
    }
    let mut tally: Vec<(String, usize)> = counts
        .into_iter()
        .map(|(reason, n)| (reason.to_owned(), n))
        .collect();
    tally.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    tally
}

/// The one reason every crossing was skipped for, when there is only one.
fn one_reason(skips: &[(String, usize)]) -> Option<String> {
    match skips {
        [(reason, _)] => Some(reason.clone()),
        _ => None,
    }
}

/// `; 3 skipped (2 check timed out after 120 s, 1 needs network)`, or nothing
/// when nothing was skipped.
fn skip_note(skips: &[(String, usize)]) -> String {
    let total: usize = skips.iter().map(|(_, n)| n).sum();
    if total == 0 {
        return String::new();
    }
    let parts: Vec<String> = skips
        .iter()
        .map(|(reason, n)| format!("{n} {reason}"))
        .collect();
    format!("; {total} skipped ({})", parts.join(", "))
}

/// The middle value of a sorted copy — the upper of the two on an even count.
fn median(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    Some(sorted[sorted.len() / 2])
}
```

Delete the old literal trailer from `render_codebase` — the line
`out.push_str("             tiers 6-7 skipped: slice B2 (--allow-exec)\n");` is replaced by `exec_trailer`. Update any existing test asserting the old string (search `mod tests` for `slice B2`) to the new `--allow-exec not given`.

- [ ] **Step 5: Add the lift's two columns**

In `lift_line` (`:860`), after the `symbols_delta` block:

```rust
    cells.extend(exec_delta_cells(&pairs));
```

and below `symbols_delta` (`:928`):

```rust
/// The exec tiers' lift: `compile +0.33` and `test n/a`.
///
/// A pair contributes only when BOTH arms produced a verdict for that tier —
/// a difference against a skip is not a measurement, and would read as a lift
/// of exactly the arm that ran. Nothing at all when neither arm ever ran the
/// exec tiers.
fn exec_delta_cells(pairs: &[(&CodebaseRow, &CodebaseRow)]) -> Vec<String> {
    if !pairs.iter().any(|(a, b)| a.exec.is_some() || b.exec.is_some()) {
        return Vec::new();
    }
    [
        ("compile", exec_delta(pairs, |e| &e.compile)),
        ("test", exec_delta(pairs, |e| &e.test)),
    ]
    .into_iter()
    .map(|(label, delta)| match delta {
        Some(value) => format!("{label} {value:+.2}"),
        None => format!("{label} n/a"),
    })
    .collect()
}

fn exec_delta(
    pairs: &[(&CodebaseRow, &CodebaseRow)],
    pick: fn(&ExecRow) -> &ExecScore,
) -> Option<f64> {
    let deltas: Vec<f64> = pairs
        .iter()
        .filter_map(|(a, b)| {
            match (pick(a.exec.as_ref()?), pick(b.exec.as_ref()?)) {
                (ExecScore::Value(x), ExecScore::Value(y)) => Some(y - x),
                _ => None,
            }
        })
        .collect();
    if deltas.is_empty() {
        return None;
    }
    Some(deltas.iter().sum::<f64>() / as_f64(deltas.len()))
}
```

- [ ] **Step 6: Write the failing test for the lift columns**

In `mod tests`:

```rust
    /// The lift's exec columns come from the pairs measured in BOTH arms.
    #[test]
    fn the_context_lift_reports_the_exec_tiers_too() {
        let eval = scratch("codebase-exec-lift");
        let mut writer = RunWriter::create(&eval, "r35-model", &exec_head()).expect("create");
        for (id, arm, compile) in [
            ("cross_file_first-abc123-L2", "no_extra", 0.0),
            ("cross_file_first-abc123-L2+extra", "extra", 1.0),
        ] {
            let mut task = cross_arm(id, arm, "let a = build(1);");
            if let Some(row) = task.codebase.as_mut() {
                row.exec = Some(super::ExecRow {
                    compile: super::ExecScore::Value(compile),
                    compile_error: None,
                    tests: Vec::new(),
                    test: super::ExecScore::Skipped("no covering test".into()),
                    test_failure: None,
                    check_secs: 6.0,
                    test_secs: 0.0,
                });
            }
            writer.append(task).expect("append");
        }
        let block = super::render_codebase(&RunLog::load(writer.dir()).expect("load"));
        assert!(block.contains("compile +1.00  test n/a"), "{block}");
    }
```

- [ ] **Step 7: Run every store test and watch them pass**

Run: `cargo test --locked --lib bench::store::tests`
Expected: PASS — the five new tests plus every existing one, with the old `slice B2 (--allow-exec)` assertion updated in place.

- [ ] **Step 8: Run the floor and commit**

Run: `cargo fmt && cargo clippy --locked --all-targets -- -D warnings && cargo test --locked`
Expected: clean; all tests pass.

```bash
git add src/core/bench/store.rs && git commit -m "$(cat <<'EOF'
feat(bench): the report's two exec cells per tier, the two lift columns, and the timing trailer

compile's n counts crossings with a verdict; a skip is excluded from the
mean and counted in the trailer by reason, so a machine that could not
ask the question never scores the model down for it. The trailer has
three shapes and the rows decide which.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01YDaTTHJySW2ix3P85BcP6W
EOF
)"
```

---

### Task 7: The estimate, the dry-run line, the head, and the docs

**Files:**
- Modify: `src/commands/capability.rs:847-871` (`codebase_plan_line`), `:873-879` (`codebase_estimate_secs`), `:899-911` (`bench_estimate`), `:929-961` (`bench`), `:994-1008` (`head_inputs`), `:1511-1520` (`HeadInputs` + the two accessors from Task 1), `:1573-1594` (`build_head`)
- Modify: `README.md:102-103` (the two command rows), `:119-158` (the codebase paragraph), `:388-389` (the error table)
- Modify: `CHANGELOG.md` `[Unreleased] / ### Added`
- Modify: `IDEAS.md:134` (the status line)
- Modify: `docs/capability-spec.md:897` (the status line) and `:901`, `:931-932` (the pointers)

**Interfaces:**
- Consumes: `BenchArgs.allow_exec` (Task 1); `Prepared.exec`, `Exec::{allowed, cargo_version, finish}` (Tasks 2, 5).
- Produces: nothing later tasks depend on except the finished command surface.

- [ ] **Step 1: Write the failing tests for the estimate and the plan line**

In `src/commands/capability.rs`'s `mod tests`, extend the existing `the_plan_line_names_every_tier_and_the_second_arm` neighbourhood with:

```rust
    #[test]
    fn the_plan_line_and_the_estimate_both_name_the_exec_cost() {
        let mut prepared = codebase_prepared_fixture();
        prepared.counts = crate::core::bench::codebase::Counts {
            in_file: 12,
            function_body: 6,
            cross_file_first: 6,
        };
        // Without the flag: 12 + 6 + 2*6 = 30 crossings at 6 s.
        assert_eq!(super::codebase_estimate_secs(&prepared, false), 180);
        // With it: another 6 s of cargo per crossing.
        assert_eq!(super::codebase_estimate_secs(&prepared, true), 360);

        let off = super::codebase_plan_line(&prepared, std::path::Path::new("/r"), false);
        assert!(!off.contains("exec"), "{off}");
        let on = super::codebase_plan_line(&prepared, std::path::Path::new("/r"), true);
        assert!(
            on.contains("+ exec: cold check unmeasured, then ~6 s per crossing"),
            "{on}"
        );
    }
```

`codebase_prepared_fixture()` is a small helper this step adds beside the existing capability-test fixtures: a `Prepared` with `exec: Exec::Off`, empty tasks/shortfall, `head: "4818813deeaa…"`, `set_hash: "abcdef123456"`, `symbols: Symbols::default()`, `cfg_test_lines: 0`, `cfg_test_files: 0`. If a fixture of that shape already exists in the module, reuse it rather than adding a second.

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test --locked --lib commands::capability::tests::the_plan_line_and_the_estimate_both_name_the_exec_cost`
Expected: FAIL — `this function takes 1 argument but 2 arguments were supplied`.

- [ ] **Step 3: Widen the estimate and the plan line**

In `src/commands/capability.rs`, `codebase_estimate_secs` (`:875`):

```rust
/// Six seconds per CROSSING, not per task: a cross-file task is crossed
/// twice, so the estimate is `(in_file + function_body + 2 × cross) × 6` —
/// doubled under `--allow-exec`, where each crossing also pays for a check.
///
/// Six seconds for a warm incremental check is a guess, and the run replaces
/// it with the measured pair as soon as it has two of them.
fn codebase_estimate_secs(
    prepared: &crate::core::bench::codebase::Prepared,
    allow_exec: bool,
) -> u64 {
    let c = prepared.counts;
    let crossings = c.in_file + c.function_body + 2 * c.cross_file_first;
    let per_crossing = if allow_exec { 12 } else { 6 };
    u64::try_from(crossings).unwrap_or(0) * per_crossing
}
```

`codebase_plan_line` (`:850`) gains the same third argument and one clause, appended after the shortfall:

```rust
fn codebase_plan_line(
    prepared: &crate::core::bench::codebase::Prepared,
    repo: &std::path::Path,
    allow_exec: bool,
) -> String {
    // … head12 / census / elided / shortfall unchanged …
    let exec = if allow_exec {
        " + exec: cold check unmeasured, then ~6 s per crossing"
    } else {
        ""
    };
    format!(
        "codebase: {} tasks from {} @ {head12} ({census}){elided}{shortfall}{exec}\n",
        prepared.tasks.len(),
        repo.display()
    )
}
```

Update the two call sites: `bench_estimate` (`:907`)

```rust
    let codebase_secs = inputs
        .prepared
        .map_or(0, |p| codebase_estimate_secs(p, inputs.args.allow_exec));
```

and `render_dry_run` (`:922`)

```rust
    if let (Some(p), Some(repo)) = (inputs.prepared, inputs.args.codebase) {
        out.push_str(&codebase_plan_line(p, repo, inputs.args.allow_exec));
    }
```

- [ ] **Step 4: Run it and watch it pass**

Run: `cargo test --locked --lib commands::capability::tests`
Expected: PASS, including the existing `the_plan_line_names_every_tier_and_the_second_arm` (its call site gains a `false`).

- [ ] **Step 5: Wire the stamp's exec fields to the prepared run**

Replace `HeadInputs.codebase` (`:1517-1519`) and the two placeholder accessors from Task 1:

```rust
/// The codebase run's identity and the environment its exec tiers ran in.
struct CodebaseHead<'a> {
    head: &'a str,
    set_hash: &'a str,
    allow_exec: bool,
    cargo_version: Option<&'a str>,
}

struct HeadInputs<'a> {
    props: crate::core::bench::runner::PropsInfo,
    plan: &'a crate::core::bench::sweep::SweepPlan,
    fixture: Option<&'a std::path::Path>,
    suite: Option<crate::core::bench::lifecycle::Suite>,
    /// The codebase run, when there is one — drives `corpus_id` ahead of
    /// `suite`/`fixture`, and the three exec fields.
    codebase: Option<CodebaseHead<'a>>,
}

impl HeadInputs<'_> {
    /// Whether this run was allowed to execute the repository. A run that
    /// executed it and one that only read it are not the same environment.
    fn allow_exec(&self) -> bool {
        self.codebase.as_ref().is_some_and(|c| c.allow_exec)
    }

    /// The `cargo --version` line, when exec actually ran — `None` both
    /// without the flag and on a machine with no toolchain.
    fn cargo_version(&self) -> Option<&str> {
        self.codebase.as_ref().and_then(|c| c.cargo_version)
    }
}
```

`head_corpus` (`:1554`) reads the two renamed fields:

```rust
    let corpus = match inputs.codebase.as_ref() {
        Some(c) => codebase_corpus_id(c.head, c.set_hash),
        None => corpus_id(inputs.suite, inputs.fixture)?,
    };
```

and `head_inputs` (`:994`) builds the struct:

```rust
        codebase: inputs.prepared.map(|p| CodebaseHead {
            head: p.head.as_str(),
            set_hash: p.set_hash.as_str(),
            allow_exec: p.exec.allowed(),
            cargo_version: p.exec.cargo_version(),
        }),
```

- [ ] **Step 6: Write the failing test that the stamp records what ran**

In `mod tests`:

```rust
    /// The stamp says what the run was allowed to do, and — when it did it —
    /// which toolchain did it.
    #[test]
    fn the_head_records_the_exec_environment_only_when_exec_ran() {
        let off = super::HeadInputs {
            props: props_fixture(),
            plan: &plan_fixture(),
            fixture: None,
            suite: None,
            codebase: Some(super::CodebaseHead {
                head: "4818813deeaa",
                set_hash: "abcdef123456",
                allow_exec: false,
                cargo_version: None,
            }),
        };
        assert!(!off.allow_exec());
        assert_eq!(off.cargo_version(), None);

        let on = super::HeadInputs {
            codebase: Some(super::CodebaseHead {
                head: "4818813deeaa",
                set_hash: "abcdef123456",
                allow_exec: true,
                cargo_version: Some("cargo 1.95.0"),
            }),
            ..off
        };
        assert!(on.allow_exec());
        assert_eq!(on.cargo_version(), Some("cargo 1.95.0"));
    }
```

(`props_fixture()`/`plan_fixture()` are whatever the module's existing head tests already use; reuse them rather than adding new ones. If `HeadInputs` is not `..`-spreadable because `props` is not `Clone`, build the second value in full.)

- [ ] **Step 7: Run it and watch it pass**

Run: `cargo test --locked --lib commands::capability::tests::the_head_records_the_exec_environment_only_when_exec_ran`
Expected: PASS.

- [ ] **Step 8: Remove the worktree when the run ends**

In `bench` (`:929`), the candidate loop moves into a helper so `prepared` can be consumed after it:

```rust
fn bench(ctx: &Ctx, args: &BenchArgs) -> Result<ExitCode, ChekovError> {
    use crate::core::bench::{lifecycle, sweep};
    // The user's own repository is asked about first: a dirty tree is refused
    // before a single question about servers or models is asked.
    let prepared = prepare_codebase(ctx, args)?;
    let candidates = resolve_candidates(ctx, args)?;
    let inputs = RunInputs {
        args,
        prepared: prepared.as_ref(),
    };
    let plan: sweep::SweepPlan = (&ctx.config.file.bench).into();
    let steps = bench_steps(ctx, &candidates);
    let estimate = bench_estimate(&steps, &plan, &inputs)?;
    if args.dry_run {
        print!("{}", render_dry_run(&steps, estimate, &inputs));
        return finish_codebase(prepared).map(|()| ExitCode::SUCCESS);
    }
    if lifecycle::needs_confirm(&steps) {
        super::confirm(
            &format!(
                "bench {} candidate(s) with launch + teardown, ~{} min estimated",
                steps.len(),
                estimate.div_ceil(60)
            ),
            args.yes,
        )?;
    }
    let outcome = run_candidates(ctx, &candidates, &inputs);
    finish_codebase(prepared)?;
    outcome
}

/// Every candidate, each one's run directory printed as it lands.
fn run_candidates(
    ctx: &Ctx,
    candidates: &[(
        crate::core::registry::Effective,
        crate::core::bench::lifecycle::StepAction,
    )],
    inputs: &RunInputs,
) -> Result<ExitCode, ChekovError> {
    for candidate in candidates {
        let dir = run_candidate(ctx, candidate, inputs)?;
        println!("run: {}", dir.display());
    }
    Ok(ExitCode::SUCCESS)
}

/// The worktree and the scratch target directory, removed with the run.
///
/// Explicit rather than left to `Worktree::drop`, so a cleanup that fails is
/// reported. Without `--allow-exec` there is nothing here to remove: `prepare`
/// took the worktree away before it returned.
fn finish_codebase(
    prepared: Option<crate::core::bench::codebase::Prepared>,
) -> Result<(), ChekovError> {
    match prepared {
        Some(p) => p.exec.finish(),
        None => Ok(()),
    }
}
```

> `finish_codebase` runs **after** `run_candidates` returns, and its `?` is placed so a cleanup failure surfaces even when the run succeeded — but a run that failed keeps its own error, because `outcome` is returned last. If the borrow checker objects to moving `prepared` while `inputs` is alive, add `drop(inputs);` before the call: `inputs` is not used again.

- [ ] **Step 9: Run the whole suite**

Run: `cargo test --locked`
Expected: PASS.

- [ ] **Step 10: Update the README**

Three edits.

(a) `README.md:102`, the `capability bench` row's synopsis — add `[--allow-exec]` to the flag list in the first cell.

(b) `README.md:103`, the `--codebase` row — replace `tiers 6–7 are slice B2.` with:

```
tiers 6–7 (compile gate, covering test) run only under `--allow-exec`, which is the single gate on every path that executes repository code.
```

(c) After the codebase paragraph's last sentence (`README.md:158`), add a new paragraph:

```markdown
**`--allow-exec`** turns on tiers 6 and 7. Tier 6 splices the fill into the
worktree's copy of the file, runs `cargo check --message-format=json
--offline`, and passes when the JSON stream carries no `error` diagnostic
anywhere in the workspace — a fill that breaks a caller in another file fails,
which is the point of the cross-file tier. Tier 7 then runs the repository's
own tests for the masked symbol: the enclosing function's name (plus the
cross-file symbol, when there is one), the nearest `Cargo.toml` above the file
for the crate, up to five `#[test]` functions in that crate whose bodies name
the symbol as a whole word — `tests/*.rs` included — and `cargo test -p <crate>
--offline -- <t> --exact` for each. Tier 7 passes only when every candidate
passes. **This runs the repository's code.** `cargo check` and `cargo test`
execute its `build.rs` scripts, its proc-macros and its tests — the same trust
as building the repository yourself. chekov bounds it and does not sandbox it:
the detached worktree is the only place written (your checkout is never
touched), one `cargo fetch` before the loop is the only networked step and
every invocation after it carries `--offline`, `CARGO_TARGET_DIR` points at
`eval/.scratch/target-<head12>` so nothing lands in the repository's own
`target/`, each check gets 120 seconds and each test run 300 with the whole
process group killed at the deadline, and every crossing is reverted with `git
checkout --` and the bytes compared before the next one starts — a worktree
that will not restore stops the run rather than measuring against a file
nobody can vouch for. Nothing here is a silent zero: a missing toolchain, an
offline registry, a timeout, a span outside every function, a crate with no
covering test are each a counted, printed reason, and the report's `compile`
mean is taken over crossings with a verdict only. Only Rust is implemented;
the module is shaped for `tsc --noEmit` and `python -m py_compile` behind the
same gate. Without the flag the ladder stops at tier 5 and the trailer reads
`tiers 6-7 skipped: --allow-exec not given`. Because the stamp records
`allow_exec`, `cargo_version` and `exec_target`, `compare` refuses across a run
that executed and one that did not — they are different environments.
```

(d) `README.md:388-389`, the error table — add a row:

```
| `ExecWorktreeDirty` from `capability bench --codebase --allow-exec` | A `git checkout --` did not restore the file tier 6 spliced. The run stopped rather than measure the next crossing against a file it cannot vouch for. Inspect the worktree the message names, delete it (`git worktree remove --force <path>`, then `git worktree prune`), and resume with `--resume <RUN>`: every row up to that crossing is intact. |
```

- [ ] **Step 11: Update the CHANGELOG**

In `CHANGELOG.md`, at the top of `[Unreleased] / ### Added`:

```markdown
- `capability bench --codebase` gains `--allow-exec` and, behind it, the two
  tiers that say whether a fill is code rather than plausible text. Tier 6
  splices the fill (trimmed to the gold's line count — the same text tiers 1-4
  grade) into the worktree's copy of the file, runs `cargo check
  --message-format=json --offline`, and passes when the stream carries no
  `error` anywhere in the workspace; the exit status is not the verdict,
  because cargo exits non-zero for things it also reports and the diagnostics
  are the auditable record. Tier 7 runs the repository's own covering tests
  for the masked symbol — the enclosing function plus the cross-file symbol,
  the nearest `Cargo.toml` with a `[package] name`, up to five `#[test]`
  functions naming it as a whole word outside literals (`tests/*.rs`
  included), each through `cargo test -p <crate> --offline -- <t> --exact` —
  and passes only when all of them pass. The bounds are stated and enforced:
  the detached worktree is the only place written, one `cargo fetch` before
  the loop is the only networked step, `CARGO_TARGET_DIR` is
  `eval/.scratch/target-<head12>`, 120 s per check and 300 s per test run with
  a process-group kill, and every crossing is reverted and byte-compared
  before the next — a worktree that will not restore raises
  `ExecWorktreeDirty` and stops the run, with the rows written so far intact
  and resumable. Nothing degrades silently: no toolchain, an offline registry,
  a timeout, a span outside every function, a crate with no covering test are
  each a counted reason, printed in the block's trailer by reason, and
  excluded from the `compile` mean rather than averaged in as zeros. The row
  gains `exec` (`#[serde(default)]`; pre-B2 rows load as `None`), the stamp
  gains `allow_exec`, `cargo_version` and `exec_target` — so `compare` refuses
  across a run that executed the repository and one that did not — and the
  report gains two cells per tier line, two lift columns and the timing
  trailer. The task set is unchanged, so `corpus_id` is unchanged. Only Rust;
  `--judge` is slice C.
```

- [ ] **Step 12: Update IDEAS.md and the umbrella spec**

`IDEAS.md:134` — replace `slices B2 (exec tiers behind --allow-exec) and C (--judge) OPEN` with `slice B2 (exec tiers behind --allow-exec) SHIPPED 2026-08-30; slice C (--judge) OPEN`.

`docs/capability-spec.md:897` — replace the status sentence with:

```
Status 2026-08-30: slices A (Rust, same-file, tiers 1–5), B1 (`cross_file_first` with `input_extra`, two arms and the context lift) and B2 (tiers 6–7 behind `--allow-exec`) shipped — see `docs/superpowers/specs/2026-08-29-codebase-mode-slice-a-design.md`, `…-slice-b1-design.md` and `2026-08-30-codebase-mode-slice-b2-design.md`; slice C (`--judge`) open.
```

`docs/capability-spec.md:901`, at the end of the **Safety gate first** paragraph, append:

```
Implemented for Rust in slice B2 (`2026-08-30-codebase-mode-slice-b2-design.md`): one `cargo fetch` then `--offline`, a scratch `CARGO_TARGET_DIR`, 120 s/300 s wall clocks with a process-group kill, and a revert verified byte for byte after every crossing — `ExecWorktreeDirty` stops the run.
```

`docs/capability-spec.md:931-932`, append to each of the two ladder items:

- item 6: ` — Rust shipped in slice B2; the JSON diagnostics are the verdict, not the exit status, and an \`error\` anywhere in the workspace counts.`
- item 7: ` — shipped in slice B2: the enclosing function (plus the cross-file symbol), the nearest \`[package]\`, up to five \`#[test]\` functions naming it as a whole word, \`tests/*.rs\` included.`

- [ ] **Step 13: Check the README-equals-defaults test still holds**

Run: `cargo test --locked`
Expected: PASS. `[bench] codebase_tasks` is unchanged, so the README/`config.example.toml` agreement test is untouched; if it fails, the README edit above disturbed the config block and the fix is in the README, not the test.

- [ ] **Step 14: Run the floor and commit**

Run: `cargo fmt && cargo clippy --locked --all-targets -- -D warnings && cargo test --locked`
Expected: clean; all tests pass.

```bash
git add src/commands/capability.rs README.md CHANGELOG.md IDEAS.md docs/capability-spec.md && git commit -m "$(cat <<'EOF'
feat(capability): --allow-exec's estimate, dry-run clause, stamp wiring, and the docs that say what it runs

The README says plainly that this executes the repository's build.rs,
proc-macros and tests, and lists the bounds that are enforced rather than
implying a sandbox that is not there.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01YDaTTHJySW2ix3P85BcP6W
EOF
)"
```

---

### Task 8: The real-toolchain integration test, and the live runs

Everything before this task runs against a shell script. This one runs against a real `cargo`, once, behind an env gate — and then the two live model runs that are the branch's evidence.

**Files:**
- Create: `tests/codebase_exec.rs`
- Modify: `README.md` (one line in the contributor/test section naming the env gate) and `CONTRIBUTING.md` if it lists the test commands

**Interfaces:**
- Consumes: the whole public surface — `codebase::{prepare, PrepareInputs, Prepared}`, `codebase::exec::{Exec, Env, exec_crossing, Timeouts}`, `store::{ExecScore, ExecRow}`.
- Produces: nothing.

**Why it is gated.** A real `cargo check` on a fresh crate is tens of seconds and needs a toolchain and a writable `CARGO_HOME`; `make test` must stay fast and must pass on a machine that has neither. `CHEKOV_TEST_EXEC=1` opts in, and the test **prints why it skipped** when the variable is absent — a test that quietly passes for the wrong reason is worse than one that says it did not run.

- [ ] **Step 1: Write the integration test**

Create `tests/codebase_exec.rs`:

```rust
//! Tiers 6 and 7 against a real `cargo`, once.
//!
//! Gated on `CHEKOV_TEST_EXEC=1`: a real check costs tens of seconds and
//! needs a toolchain, and `make test` has to pass on a machine with neither.
//! Run it with `CHEKOV_TEST_EXEC=1 cargo test --locked --test codebase_exec`.

use std::path::{Path, PathBuf};

use chekov::core::bench::codebase::exec::{self, Env, Timeouts};
use chekov::core::bench::codebase::{CodebaseTask, Excluded, TaskTier, tree};
use chekov::core::bench::store::ExecScore;

/// `true` when the caller asked for the real toolchain.
fn opted_in() -> bool {
    if std::env::var("CHEKOV_TEST_EXEC").as_deref() == Ok("1") {
        return true;
    }
    eprintln!("skipping: set CHEKOV_TEST_EXEC=1 to run the exec tiers against a real cargo");
    false
}

/// One crate, two functions, one of them covered by a test.
const LIB_RS: &str = "\
pub fn alpha(n: u32) -> u32 {
    let doubled = n * 2;
    doubled
}

pub fn beta(n: u32) -> u32 {
    n + 1
}

#[cfg(test)]
mod tests {
    #[test]
    fn covers_alpha() {
        assert_eq!(super::alpha(2), 4);
    }
}
";

const MANIFEST: &str = "[package]\nname = \"widget\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n";

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("chekov-it-exec").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    dir
}

/// A committed one-crate repository, plus extra files the caller wants.
fn repo(dir: &Path, extra: &[(&str, &str)]) -> PathBuf {
    let repo = dir.join("repo");
    std::fs::create_dir_all(repo.join("src")).expect("src");
    std::fs::write(repo.join("Cargo.toml"), MANIFEST).expect("manifest");
    std::fs::write(repo.join("src/lib.rs"), LIB_RS).expect("lib.rs");
    for (path, text) in extra {
        let full = repo.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("parent");
        }
        std::fs::write(full, text).expect("extra");
    }
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "t@t"],
        vec!["config", "user.name", "t"],
        vec!["add", "-A"],
        vec!["commit", "-qm", "fixture"],
    ] {
        tree::git(&repo, &args, "fixture").expect("git");
    }
    repo
}

/// A `CodebaseTask` masking `let doubled = n * 2;` in `src/lib.rs`.
fn task_on(worktree: &Path, symbol: Option<&str>, needle: &str) -> CodebaseTask {
    let text = std::fs::read_to_string(worktree.join("src/lib.rs")).expect("lib.rs");
    let at = text.find(needle).expect("the span");
    CodebaseTask {
        id: "in_file-fixture-L2".into(),
        tier: TaskTier::InFile,
        file: "src/lib.rs".into(),
        line: 2,
        byte_range: at..at + needle.len(),
        gold: needle.to_owned(),
        prefix: text[..at].to_owned(),
        suffix: text[at + needle.len()..].to_owned(),
        excluded: Excluded {
            doc_comment: 0,
            cross_file: "n/a: same-file".into(),
            cfg_test_lines: 0,
            cross_file_withheld: 0,
        },
        name: symbol.map(str::to_owned),
        also_first_uses: Vec::new(),
        extra: None,
        extra_text: String::new(),
    }
}

fn env_over(dir: &Path, repo: &Path, timeouts: Timeouts) -> Env {
    let worktree = tree::Worktree::add(repo, &dir.join("tree")).expect("worktree");
    Env {
        worktree,
        target_dir: dir.join("target"),
        cargo_version: "real".to_owned(),
        timeouts,
    }
}

#[test]
fn a_correct_fill_passes_both_tiers_and_names_the_test_it_ran() {
    if !opted_in() {
        return;
    }
    let dir = scratch("correct");
    let repo = repo(&dir, &[]);
    let env = env_over(&dir, &repo, Timeouts::DEFAULT);
    let task = task_on(&env.worktree.path, None, "let doubled = n * 2;");
    let original = std::fs::read_to_string(env.worktree.path.join("src/lib.rs")).expect("read");
    let row = exec::exec_crossing(&env, &task, "let doubled = n * 2;").expect("the crossing runs");
    assert_eq!(row.compile, ExecScore::Value(1.0), "{row:?}");
    assert_eq!(row.tests, vec!["covers_alpha".to_owned()]);
    assert_eq!(row.test, ExecScore::Value(1.0), "{row:?}");
    assert!(row.check_secs > 0.0 && row.test_secs > 0.0);
    assert_eq!(
        std::fs::read_to_string(env.worktree.path.join("src/lib.rs")).expect("read"),
        original
    );
    env.finish().expect("cleanup");
}

#[test]
fn a_type_error_fails_six_stores_the_message_and_skips_seven() {
    if !opted_in() {
        return;
    }
    let dir = scratch("type-error");
    let repo = repo(&dir, &[]);
    let env = env_over(&dir, &repo, Timeouts::DEFAULT);
    let task = task_on(&env.worktree.path, None, "let doubled = n * 2;");
    let row = exec::exec_crossing(&env, &task, "let doubled = \"two\";")
        .expect("the crossing runs");
    assert_eq!(row.compile, ExecScore::Value(0.0), "{row:?}");
    let message = row.compile_error.expect("the first error is stored");
    assert!(message.contains("src/lib.rs:"), "{message}");
    assert!(message.contains("mismatched types"), "{message}");
    assert_eq!(
        row.test,
        ExecScore::Skipped("did not compile".to_owned()),
        "{row:?}"
    );
    env.finish().expect("cleanup");
}

#[test]
fn a_fill_in_the_untested_function_is_skipped_for_want_of_a_covering_test() {
    if !opted_in() {
        return;
    }
    let dir = scratch("untested");
    let repo = repo(&dir, &[]);
    let env = env_over(&dir, &repo, Timeouts::DEFAULT);
    let task = task_on(&env.worktree.path, None, "n + 1");
    let row = exec::exec_crossing(&env, &task, "n + 1").expect("the crossing runs");
    assert_eq!(row.compile, ExecScore::Value(1.0), "{row:?}");
    assert_eq!(
        row.test,
        ExecScore::Skipped(exec::NO_COVERING_TEST.to_owned()),
        "beta has no test naming it"
    );
    assert!(row.tests.is_empty());
    env.finish().expect("cleanup");
}

/// A `build.rs` that sleeps past the ceiling: a skip with the reason, and the
/// file still restored. The ceiling is lowered through `Env.timeouts`, which
/// is why it lives on the environment rather than in a constant.
#[test]
fn a_build_script_that_sleeps_past_the_ceiling_is_a_skip_and_the_file_comes_back() {
    if !opted_in() {
        return;
    }
    let dir = scratch("timeout");
    let repo = repo(
        &dir,
        &[("build.rs", "fn main() { std::thread::sleep(std::time::Duration::from_secs(120)); }\n")],
    );
    let env = env_over(
        &dir,
        &repo,
        Timeouts {
            check: std::time::Duration::from_secs(5),
            test: std::time::Duration::from_secs(5),
        },
    );
    let task = task_on(&env.worktree.path, None, "let doubled = n * 2;");
    let original = std::fs::read_to_string(env.worktree.path.join("src/lib.rs")).expect("read");
    let row = exec::exec_crossing(&env, &task, "let doubled = n * 2;").expect("the crossing runs");
    let ExecScore::Skipped(reason) = row.compile else {
        panic!("expected a skip, got {:?}", row.compile);
    };
    // The seconds in the message are the CONFIGURED ceiling — 5 here, 120 in
    // production — so the message is true rather than a copied constant.
    assert!(reason.starts_with("check timed out after "), "{reason}");
    assert!(reason.ends_with(" s"), "{reason}");
    assert_eq!(
        std::fs::read_to_string(env.worktree.path.join("src/lib.rs")).expect("read"),
        original,
        "a killed check still gives the file back"
    );
    env.finish().expect("cleanup");
}
```

- [ ] **Step 2: Run it both ways**

Run: `cargo test --locked --test codebase_exec`
Expected: PASS (4 passed), with four `skipping: set CHEKOV_TEST_EXEC=1 …` lines on stderr — nothing was actually executed.

Run: `CHEKOV_TEST_EXEC=1 cargo test --locked --test codebase_exec -- --test-threads=1 --nocapture`
Expected: PASS (4 passed), in a minute or two. `--test-threads=1` because the four tests share a `CARGO_HOME` registry lock.

- [ ] **Step 3: Add the anything-but-public exports the test needs**

`cargo test --locked --test codebase_exec` reaches the crate through `lib.rs`, so `codebase::exec`, `codebase::tree`, `CodebaseTask`, `Excluded`, `TaskTier`, `ExecScore` must all be `pub` on that path. `exec` and `tree` are already `pub mod`; if the compiler reports `Worktree::add` or `tree::git` as private (`git` was made `pub(super)` in Task 2), add a narrow public seam rather than widening `git`:

```rust
impl Worktree {
    /// Commit a fixture repository from an integration test.
    ///
    /// `git` itself stays `pub(super)` — the module's error contract is not
    /// something a caller outside it should be able to spell — and this is the
    /// one shape a test needs.
    #[doc(hidden)]
    pub fn run_git_for_test(repo: &Path, args: &[&str]) -> Result<(), ChekovError> {
        git(repo, args, "test fixture").map(|_| ())
    }
}
```

and use `tree::Worktree::run_git_for_test(&repo, &args)` in the fixture instead of `tree::git`.

- [ ] **Step 4: Document the gate**

In `CONTRIBUTING.md` beside the existing test commands (or, if it has none, in `README.md`'s contributor section):

```markdown
`make test` never executes a benchmarked repository's build. The one test
that does — tiers 6-7 against a real `cargo` — is behind an env gate and
prints why it skipped:

    CHEKOV_TEST_EXEC=1 cargo test --locked --test codebase_exec -- --test-threads=1
```

- [ ] **Step 5: Run the floor and commit**

Run: `cargo fmt && cargo clippy --locked --all-targets -- -D warnings && cargo test --locked`
Expected: clean; all tests pass, with the four integration tests skipping out loud.

```bash
git add tests/codebase_exec.rs CONTRIBUTING.md README.md src/core/bench/codebase/tree.rs && git commit -m "$(cat <<'EOF'
test(codebase): tiers 6-7 against a real cargo, behind CHEKOV_TEST_EXEC=1

Four cases from the spec: a correct fill passing both tiers with the
covering test named, a type error failing 6 with the message stored and 7
skipped, the untested function skipped for want of a covering test, and a
build.rs that sleeps past a lowered ceiling — skipped, with the file
verified restored. It prints why it skipped when the gate is unset: a
test that quietly passes for the wrong reason is worse than one that says
it did not run.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01YDaTTHJySW2ix3P85BcP6W
EOF
)"
```

- [ ] **Step 6: The live run on this repository**

The branch's evidence. **Clean clones only** — the gate refuses a dirty tree, and `--allow-exec` writes into a worktree of whatever HEAD it finds.

```bash
git clone /Users/amoscoletti/personal_dev/chekov /tmp/chekov-live && \
  chekov capability bench --codebase /tmp/chekov-live --models ornith-1.5-35b-a3b --allow-exec --yes
```

Expected: the codebase block with `; exec: cargo <version>, offline, scratch target` in the header, `compile`/`test` cells on the `in_file`, `function_body` and both cross-file lines, `compile` and `test` on the `context lift` line, and a `tiers 6-7: cold check <s> s, then <s> s median per crossing` trailer. Save the block verbatim.

Sanity checks before recording it, each of which has caught a real defect in this design:
- `git -C /tmp/chekov-live status --porcelain` is **empty** afterwards — the worktree was separate and the clone was never written.
- `ls /tmp/chekov-live/target` does not exist or is untouched — the build went to `eval/.scratch/`.
- `git -C /tmp/chekov-live worktree list` shows only the main tree — `finish` removed the detached one.
- The `compile` mean's `n` plus the trailer's skip count equals the crossing count on that line.

- [ ] **Step 7: The live run on pushkin**

```bash
git clone /Users/amoscoletti/personal_dev/chekov/pushkin /tmp/pushkin-live && \
  chekov capability bench --codebase /tmp/pushkin-live --models ornith-1.5-35b-a3b --allow-exec --yes
```

(Use pushkin's real origin if the path above is not a repository root.) Expected: the same shape. A workspace whose root `Cargo.toml` has no `[package]` will show tier 7 as `no crate` for files under it — record that rather than working around it; it is what the spec says happens.

- [ ] **Step 8: Write the PR body to disk — no push, no PR**

Write `/tmp/b2-pr-body.md` with: the one-paragraph summary; both live blocks verbatim under their repository names and model; the honesty lines —

- `the task set is unchanged, so corpus_id is unchanged: B2 runs compare with B1 runs on the same HEAD`;
- `allow_exec, cargo_version and exec_target are new stamp fields, so a --allow-exec run does NOT compare with a run without it — they are different environments`;
- `tiers 1-5 are unchanged; every number in them should match the B1 run on the same HEAD`;
- `Rust only. tsc --noEmit and python -m py_compile are the same shape behind the same gate, and are not implemented`;
- `--allow-exec executes the benchmarked repository's build.rs, proc-macros and tests. It is bounded (worktree, offline, scratch target, timeouts, verified revert) and it is not a sandbox`;
- `slice C (--judge) is open`;

and the four sanity checks from Step 6 with their observed results.

Do **not** run `git push` and do **not** run `gh pr create`. The body sits on disk for the human.

---

## Self-review

Run after the plan is written, before it is executed. Recorded here so an executor can see it was done and on what.

**1. Spec coverage.** Every section of `2026-08-30-codebase-mode-slice-b2-design.md` against a task:

| Spec | Task |
|---|---|
| §2 `--allow-exec` flag, default false | 1 |
| §2 toolchain probe once per run; header says so; run proceeds | 2 (probe), 5 (`Exec::Unavailable` per crossing), 6 (trailer) |
| §2 worktree lifetime; `Drop` still fires | 5 |
| §2 `cargo fetch` once, then `--offline` | 2 (`fetch`), 3-4 (`--offline` on every later invocation) |
| §2 `CARGO_TARGET_DIR` = `<eval>/.scratch/target-<head12>`, removed with the worktree | 2 (`prepare_env`, `Env::finish`) |
| §2 dry-run `+ exec: …`; live estimate line | 7 (dry-run), 5 (`ExecTiming`) |
| §2 `corpus_id` unchanged; stamp gains three fields; compare refuses | 1 |
| §3.1 splice the original, trimmed prediction, no other file touched | 3 |
| §3.2 `cargo check --message-format=json --offline`, 120 s, process group | 2 (runner), 5 (`check_tier`) |
| §3.3 diagnostics are the verdict; warnings ignored; `file:line: message` stored | 3 |
| §3.4 revert after tier 7 or immediately; byte compare; `ExecWorktreeDirty` aborts | 1 (error), 3 (`revert`), 5 (`exec_crossing` order) |
| §3 skip reasons: timeout, failed to run, needs network | 3, 5 |
| §4 enclosing function; cross-file `name` as a second symbol | 4 (`enclosing_fn`), 5 (`tier_seven_symbols`) |
| §4 crate = nearest `[package]`; `no crate` | 4 |
| §4 candidates: `#[test]` adjacency, whole word, literals excluded, `tests/*.rs`, cap 5, names stored | 4 |
| §4 `cargo test -p <crate> --offline -- <t> --exact`, 300 s, all must pass, timeout is a skip | 4 |
| §5 `ExecRow` shape, `serde(default)`, ladder unchanged, labelled measured | 1, 6 |
| §6 two cells, `n` semantics, lift columns, trailer, header clause, three trailer shapes | 6 |
| §7 safety stated in the README | 7 |
| §8 `ExecWorktreeDirty` remediation; `--resume` re-probes; no enclosing fn; unbuildable tests; non-Rust already refused | 1, 5 (probe runs on every `prepare`, resume included), 4, 5 |
| §9 every listed test | 2-6 (unit), 8 (integration + live) |
| §10 every listed file | file-structure table |

No gaps found.

**2. Placeholder scan.** No `TBD`, no `similar to Task N`, no "add tests for the above", no "handle edge cases". Every code step carries the code. Three places deliberately say "the compiler names the sites" rather than listing them — Task 1 Step 8 (`Stamp` literals), Task 3 Step 8 (`CodebaseTask` literals), Task 5 Step 4 (`Prepared` literals) — and each names the sites it already knows plus the exact command that finds the rest; that is a procedure, not a placeholder.

**3. Type consistency.** Checked across tasks: `ExecScore`/`ExecRow` (declared Task 1, consumed Tasks 5-6, spelled `crate::core::bench::store::ExecScore` in the exec tests), `Exec`/`Env`/`Timeouts` (declared Task 2, consumed 3-5, 8), `Splice`/`revert`/`first_error`/`needs_network` (Task 3, consumed 4-5), `enclosing_fn`/`crate_of`/`covering_tests`/`run_tests`/`TestRun`/`TestVerdict` (Task 4, consumed Task 5), `PrepareInputs`/`Prepared.exec` (Task 5, consumed Task 7 and by Task 3's fixture, which carries a forward note), `CodebaseHead` (Task 7 only). `CodebaseTask.byte_range` is `std::ops::Range<usize>` at every mention. The four skip constants (`NO_ENCLOSING_FN`, `NO_CRATE`, `NO_COVERING_TEST`, `DID_NOT_COMPILE`) are declared once in Task 5 and referenced by name in Tasks 5, 6 and 8 rather than re-spelled as literals — except in Task 6's report fixtures, where a literal is the point of the exact-string test.

Five fixes made in place during this review:

- Task 3's `prepared_fixture` originally called `prepare` in the new three-field shape, which Task 5 introduces; it now carries an explicit note to write the three-argument call first and let Task 5's step update it.
- Task 1's `HeadInputs::allow_exec`/`cargo_version` were originally added in Task 7 only, leaving Task 1's `build_head` referring to methods that did not exist; Task 1 now adds them as `const fn` placeholders returning `false`/`None`, and Task 7 replaces them.
- Task 6's header assertion offered two spellings joined by `||` and told the executor to pick one — a placeholder wearing a test's clothes. `cargo --version` already begins with the word `cargo`, so the rendered clause is unambiguous and the assertion is now the single exact string.
- Task 4's `walk_crate` dropped a `#[must_use] Option` on the floor in an `else if … { continue; }`, which `-D warnings` rejects; it is now `let _ = take_rs(…)` with the reason beside it.
- Edition 2024 makes `std::env::set_var` unsafe, which every fake-cargo test in Tasks 2-5 needs. The rule — `unsafe` block, the module's `CARGO_ENV` mutex held across the test, a comment naming the mutex as the soundness argument — is now a global constraint rather than an unstated habit of the first test that happened to need it.

One thing deliberately left as it is: `$CHEKOV_CARGO` is a process-wide override, and threading the program through `Env` instead would remove both the `unsafe` and the mutex. It is not done because `probe` and `prepare_env` both need the program *before* an `Env` exists, and giving each of them a fourth parameter (or a second bundle) costs more than the mutex does. If a later slice adds a second executable override, revisit it then.
