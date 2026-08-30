# Bench Per-Candidate Lifecycle (slice-5 gap, part 2 of 3) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `chekov capability bench --models a,b` owns each candidate's server lifecycle per spec §7.3 — preflight, flag hygiene against the built binary's own `--help`, spawn with the Metal residency env var, teardown with budget-release verification — plus the §7.6 confirm/`--dry-run` gates and `cache_n` recording.

**Architecture:** A new `bench/lifecycle.rs` holds the pure pieces (flag hygiene, the plan-as-data, release policy); the orchestration lives in `commands/capability.rs` beside the existing bench flow, reusing `run::preflight` (already `pub(crate)`), a new `server::spawn_daemon_with_env`, and a new `machine::live_gpu_free`. Sequential by necessity — two large models cannot co-reside.

**Spec:** `docs/capability-spec.md` §7.3 (lifecycle), §7.6 (estimate + confirm + dry-run), §2.1 (flags). `cache_n` from §7.5's row schema (observed live: prefix caching dropped `prompt_n` 1055→516 between runs).

**Server-use rule (stated, enforced):**
- one requested model, and the running server already serves it → reuse, leave it up (today's behavior);
- any other case with a live server → refuse (`chekov stop` first) — bench never kills a server it did not start;
- no server → launch each candidate with `GGML_METAL_RESIDENCY_KEEP_ALIVE_S=5`, tear down after, verify the budget released before the next.

**Confirm rule:** required whenever the plan contains a Launch step (skippable with `--yes`); a single reuse-the-running-server run stays gate-free. `--dry-run` prints the plan as data and runs nothing.

## Global Constraints

Same as part 1 (green at every commit; ≤40 LOC/≤3 args/nesting ≤3; deny_unknown_fields; no unwrap in prod; no network in tests; no new deps). Branch: `feat/capability-bench-candidates`.

---

### Task 1: `cache_n` through the measurement path

**Files:** Modify `src/core/bench/runner.rs` (Timings), `src/core/bench/store.rs` (Measure + render), `src/commands/capability.rs` (row building).

- `Timings` gains `pub cache_n: u64`; `read_timings` reads it with `.unwrap_or(0)` (absent means nothing cached, not a missing measurement — unlike the four required numbers).
- `Measure` gains `#[serde(default)] pub cache_n: u64` (old rows load as 0).
- `run_throughput` records the per-depth MAX `cache_n`; `probe_measure` records the probe's own.
- `depth_line` appends `  cache_n {n}` when non-zero — a hot prefix cache must be visible next to the prompt_n it shrank.

Tests: canned timings with `cache_n: 512` lands in the artifact; a canned body without `cache_n` yields 0 (not an error); an old JSONL row without the field loads; render shows `cache_n` only when non-zero.

Commit: `feat(bench): record cache_n — a hot prefix cache must be visible next to the prompt_n it shrank`

---

### Task 2: flag hygiene (`bench/lifecycle.rs`)

**Files:** Create `src/core/bench/lifecycle.rs` (+ register); modify `src/error.rs`.

```rust
/// Argv flags the built binary's own `--help` does not mention. chekov tracks
/// tip-of-master with no pin, and upstream REMOVES flags behind an
/// `arg_removed()` handler that terminates startup — this catches that before
/// a spawn, not as a cryptic exit in the server log.
#[must_use]
pub fn unknown_flags(argv: &[String], help_text: &str) -> Vec<String> {
    argv.iter()
        .filter(|token| token.starts_with('-'))
        .filter(|flag| !help_text.contains(flag.as_str()))
        .cloned()
        .collect()
}
```

Error:
```rust
#[error(
    "llama-server's own --help does not list '{flag}' — a routine \
     `chekov update --engine` may have removed it upstream (removed flags \
     terminate startup); fix `extra_flags`/defaults in models.toml and re-run"
)]
BenchFlagUnknown { flag: String },
```

Production capture (same file): `pub fn server_help(engine_dir: &Path) -> Option<String>` via `std::process::Command` output on `engine::server_binary` with `--help` (exit-0 gated). A failed capture warns to stderr and skips the check — an unverifiable argv is stated, never silently trusted or silently fatal.

Tests: a help snippet containing `-m, --model FNAME` and `--flash-attn` — clean argv passes; `--draft-max` (upstream-removed) is caught; value tokens (`q8_0`, paths) are never flagged; `-np` present in help passes.

Commit: `feat(bench): flag hygiene — argv checked against the binary's own --help before a spawn`

---

### Task 3: spawn with env + budget-release probe

**Files:** Modify `src/core/server.rs`, `src/core/machine.rs`, `src/core/bench/lifecycle.rs`, `src/core/config.rs`.

- `server::spawn_daemon_with_env(cfg, eff, env: &[(&str, &str)])` — the existing body with `cmd.envs(env.iter().copied())`; `spawn_daemon` delegates with `&[]`.
- `machine::live_gpu_free(engine_dir) -> Option<u64>` — `parse_list_devices(...).map(|(_, _, free)| free)`.
- `lifecycle::MetalEnv`: `pub const METAL_RESIDENCY: (&str, &str) = ("GGML_METAL_RESIDENCY_KEEP_ALIVE_S", "5");` with the §7.3 rationale comment (Metal keeps memory wired 3 minutes by default; sequential sweeps OOM nondeterministically on the SECOND model).
- Release policy, pure + injectable:

```rust
/// Wait until the engine reports ≥ `release_pct` of the budget free again.
/// `read_free` is `machine::live_gpu_free` in production, canned in tests.
pub fn wait_budget_released(
    policy: ReleasePolicy,                       // { total_mib, release_pct, max_polls, interval }
    read_free: &mut dyn FnMut() -> Option<u64>,
) -> Result<u64, ChekovError>
```
Error: `BenchBudgetNotReleased { free_mib, want_mib }` — "Metal has not released the previous model's memory — wait a few seconds and re-run, or check for other GPU processes". `[bench] release_pct` (default 80) + `release_max_polls`/`release_interval_ms` (60 × 500ms) in config.

Tests: canned sequence `[10_000, 100_000, 190_000]` with total 228_065 @80% succeeds on the third poll; a sequence that never recovers errs naming both numbers; `None` from the reader (binary gone mid-run) errs loudly.

Commit: `feat(bench): Metal-aware spawn env and budget-release verification`

---

### Task 4: the plan as data — steps, estimate, confirm, `--dry-run`

**Files:** Modify `src/core/bench/lifecycle.rs`.

```rust
pub struct BenchStep {
    pub model: String,
    pub action: StepAction,        // UseRunning | Launch
    pub depths: Vec<u32>,
    pub weights_gib: Option<f64>,  // None renders "?" — never invented
}
#[derive(PartialEq, Eq)] pub enum StepAction { UseRunning, Launch }

/// Rough wall-clock, from the spec's measured reference rates (load ≈ 4 s/GiB,
/// prefill ≈ 150 tok/s, decode ≈ 60 tok/s) — printed as an estimate, never a
/// promise.
#[must_use] pub fn estimate_secs(steps: &[BenchStep], plan: &SweepPlan) -> u64;
#[must_use] pub fn render_plan(steps: &[BenchStep], estimate_s: u64) -> String;
#[must_use] pub fn needs_confirm(steps: &[BenchStep]) -> bool;   // any Launch
```

Tests: a UseRunning-only plan needs no confirm; any Launch does; the estimate grows with depths and weights; render names every model, its action, and the total (`~N min estimated`); unknown weights render `?` and still estimate the sweep portion.

Commit: `feat(bench): the bench plan as data — printed, estimated, confirmed`

---

### Task 5: orchestration + CLI

**Files:** Modify `src/commands/capability.rs`, `src/error.rs` if needed.

CLI: `Bench` gains `--models <LIST>` (comma `value_delimiter`), `--dry-run`, `--yes`. Parse test updated.

Flow (each helper ≤40 LOC):
1. Resolve candidates: `--models` names (each `reg.effective`?, unknown → existing `UnknownModel`), default = active model.
2. Server-use rule (above) → per-candidate `StepAction`; violation → `BenchWrongModel`-style refusal (reuse the variant, `resolved` = the requested list joined).
3. Build steps + estimate; `--dry-run` → print + exit; `needs_confirm` → `super::confirm(..., yes)`.
4. Per candidate, sequentially: `Launch` → `run::preflight` → flag hygiene (`server_help` + `unknown_flags`, first offender refuses) → `spawn_daemon_with_env(cfg, eff, &[METAL_RESIDENCY])` + `write_run_state`; both actions → existing readiness/props/head/run-dir flow (one run dir per candidate, run_id `<utc>-<model>`); `Launch` teardown → `stop_pid` + pidfile remove + `clear_run_state` + `wait_budget_released`.
5. Print each run dir; on multi-candidate, print a final list.

`--resume` composes with a single candidate only; with `--models` naming several → refuse loudly ("resume one run id, one candidate") — a resumed id pins one stamp, which pins one model.

Tests: parse test; the server-use rule as a pure function over `(running: Option<&str>, requested: &[String])` with unit tests (reuse / refuse-different / refuse-multi-with-running / launch-all).

Commit: `feat(bench): per-candidate lifecycle — bench owns launch, teardown, and the release check`

---

### Task 6: docs + live demonstration

CHANGELOG entry; IDEAS status (gap part 2 SHIPPED, part 3 probe suites OPEN). Live: `chekov capability bench --dry-run` (plan as data), then a real `--models` run from a stopped server showing launch → measure → teardown → release check, and `cache_n` visible on a warm rerun.

Commit: `docs(bench): changelog and status for the candidate lifecycle`

## Self-Review
- §7.3 items: preflight (1) reused; flag hygiene (2) Task 2; Metal env (3) Task 3; readiness (4) + props (5) already shipped; pinned probes (6) shipped; timings (7) + `cache_n` Task 1; teardown + release (8) Tasks 3/5. §7.6 estimate/confirm/dry-run Task 4/5. Not in scope (part 3+): probe suites, `--suite`, per-slot-vs-longest-probe ctx check beyond the existing exact assert, composite scoring.
- Types line up: `SweepPlan` reused; `BenchStep` consumed by estimate/render/needs_confirm; `wait_budget_released` policy from `[bench]` config.
