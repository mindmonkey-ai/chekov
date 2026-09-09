# Bench Stamp + JSONL Store (slice-5 gap, part 1 of 3) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Upgrade bench storage to the spec's honest form — a 17-field `Stamp` pinning the exact configuration, append-only JSONL task rows with `--resume`, `compare` refusing on the FIRST differing stamp field, and pinned sampling — per `docs/capability-spec.md` §7.4–§7.5.

**Architecture:** New `core/hash.rs` (hand-rolled SHA-256, house style — no new dependency), `bench/stamp.rs` (the Stamp + first-mismatch), `bench/store.rs` rewritten around a run directory `$CHEKOV_HOME/eval/<run_id>/` holding `stamp.json` + `results.jsonl` (one O_APPEND write per task). `compare` loads run dirs and refuses `BenchStampMismatch` naming the first differing field, with the kernel-nondeterminism rationale in the error text. Probes pin `temperature: 0, top_k: 1, seed` from config.

**Tech Stack:** existing crate only. Gap parts 2 (lifecycle: launch/teardown, flag hygiene, confirm/dry-run) and 3 (probe suites) are separate plans.

**Spec:** `docs/capability-spec.md` §7.4 (Stamp, refusal rationale), §7.5 (JSONL schema, stamp.json, resume), §4.2 (MachineId = sha256(model_id|memsize|brand|gpu_cores)[..12]), §7.3.6 (pinned sampling).

**Deliberate deviations (flag in PR):**
- `gguf_sha256` → `revision` (the pinned HF revision + first shard name). Hashing 35–180 GiB per run is minutes of I/O; the revision is already content-addressed upstream. Field name in the stamp: `weights_revision`.
- Flags the argv does not set are recorded as the literal string `"engine-default"` — comparable (same engine commit ⇒ same default), never invented.
- Legacy per-run `.json` files from PR #29 are not readable by the new `compare`; the format existed for one day and one file.

## Global Constraints

- `make lint` + `make test` green at every commit; functions ≤40 LOC, ≤3 args, nesting ≤3; `deny_unknown_fields` on all deserialized structs; no `unwrap()` outside tests; no network in tests; no new dependencies.
- Branch `feat/capability-bench-lifecycle` (already carries the spec-doc commit). One commit per task.

---

### Task 1: hand-rolled SHA-256 (`core/hash.rs`)

**Files:** Create `src/core/hash.rs`; modify `src/core/mod.rs`.

**Interfaces — Produces:** `pub fn sha256_hex(data: &[u8]) -> String` (64 lowercase hex chars).

Tests (NIST vectors):
```rust
#[test]
fn nist_vectors() {
    assert_eq!(
        super::sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        super::sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    // 56 bytes — crosses the padding boundary into a second block.
    assert_eq!(
        super::sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
    );
}
```

Implementation: standard FIPS 180-4 — message padding to 512-bit blocks, the 64-round compression with the K constants, big-endian words. Structure as `fn compress(state: &mut [u32; 8], block: &[u8])` + `sha256_hex` doing padding/iteration/hex — each ≤40 LOC. Use `u32::wrapping_add`/`rotate_right`; no unsafe.

Commit: `feat(hash): hand-rolled SHA-256 (house style — no new dependency)`

---

### Task 2: `machine_id`

**Files:** Modify `src/core/machine.rs`.

**Interfaces — Produces:** `pub fn machine_id(m: &Machine) -> Option<String>` — `sha256(model_id|memsize|brand|gpu_cores)[..12]` per spec §4.2; `None` when ANY component is missing (an invented id would let foreign rows compare as local).

Test:
```rust
#[test]
fn machine_id_is_stable_and_refuses_partial_identity() {
    let m = /* the existing m3_ultra-style test constructor in this file's tests */;
    let id = machine_id(&m).expect("complete identity");
    assert_eq!(id.len(), 12);
    assert_eq!(id, machine_id(&m).expect("deterministic"));
    let mut partial = m.clone();
    partial.chip = None;
    assert_eq!(machine_id(&partial), None, "an invented id is worse than none");
}
```

Implementation:
```rust
/// sha256(model_id | memsize | brand | gpu_cores), first 12 hex chars.
/// `None` when any component is unknown — a partial identity would let a
/// bench row from another machine compare as if it were this one's.
#[must_use]
pub fn machine_id(m: &Machine) -> Option<String> {
    let key = format!(
        "{}|{}|{}|{}",
        m.model.as_deref()?,
        m.memsize_bytes?,
        m.chip.as_deref()?,
        m.gpu_cores?
    );
    Some(crate::core::hash::sha256_hex(key.as_bytes())[..12].to_owned())
}
```

Commit: `feat(bench): machine identity — sha256 of what the hardware is, or nothing`

---

### Task 3: the Stamp (`bench/stamp.rs`)

**Files:** Create `src/core/bench/stamp.rs`; register in bench/mod.rs; modify `src/error.rs`.

**Interfaces — Produces:**
```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stamp {
    pub machine_id: String,
    pub engine_build_commit: String,
    pub weights_revision: String,   // "<revision>/<first_shard>" — see deviations
    pub quant: String,
    pub ctx: u32,                   // per-slot window the server LOADED
    pub n_parallel: u32,            // /props total_slots
    pub kv_unified: String,         // flag value or "engine-default"
    pub n_batch: String,
    pub n_ubatch: String,
    pub type_k: String,
    pub type_v: String,
    pub flash_attn: String,
    pub seed: u32,
    pub temperature_milli: u32,     // 0 = greedy; integer so Stamp stays Eq-able by field
    pub chekov_version: String,
    pub prompt_set_hash: String,
    pub corpus_id: String,
}

/// The FIRST differing field name, in declaration order — or None if equal.
#[must_use]
pub fn first_mismatch(a: &Stamp, b: &Stamp) -> Option<&'static str>;

/// Parse one flag's value out of a launch argv ("--cache-type-k q8_0" → "q8_0";
/// bare switches report "on"); absent → "engine-default".
#[must_use]
pub fn flag_value(args: &[String], flag: &str) -> String;
```

Error (error.rs):
```rust
#[error(
    "bench stamp mismatch on '{field}' ({a} vs {b}) — llama.cpp does not \
     guarantee bit-identical results across configurations: GPU reduction \
     kernels pick different accumulation orders and float addition is not \
     associative, so determinism holds only inside one pinned configuration; \
     re-bench under a matching stamp and compare those runs"
)]
BenchStampMismatch { field: String, a: String, b: String },
```

Tests:
```rust
fn stamp() -> Stamp { /* all fields filled with fixed values */ }

#[test]
fn identical_stamps_have_no_mismatch() {
    assert_eq!(first_mismatch(&stamp(), &stamp()), None);
}

#[test]
fn the_first_differing_field_is_named_in_declaration_order() {
    let mut b = stamp();
    b.type_k = "f16".into();
    b.engine_build_commit = "00c0ffee".into();
    // engine_build_commit precedes type_k in declaration order.
    assert_eq!(first_mismatch(&stamp(), &b), Some("engine_build_commit"));
}

#[test]
fn flag_values_parse_and_absence_is_engine_default() {
    let args: Vec<String> = ["--cache-type-k", "q8_0", "--flash-attn", "on", "-kvu"]
        .map(String::from).to_vec();
    assert_eq!(flag_value(&args, "--cache-type-k"), "q8_0");
    assert_eq!(flag_value(&args, "--flash-attn"), "on");
    assert_eq!(flag_value(&args, "-kvu"), "on", "a bare switch reports on");
    assert_eq!(flag_value(&args, "--n-batch"), "engine-default");
}
```

`first_mismatch` implementation: a macro-free chain — compare each field in declaration order, return the first name. (17 `if a.x != b.x { return Some("x") }` lines is fine and exhaustive by inspection; a helper macro is not worth it.) `flag_value`: find the flag token; if the next token exists and does not start with `-`, that is the value, else `"on"`.

Commit: `feat(bench): the 17-field Stamp — determinism holds only inside one pinned configuration`

---

### Task 4: JSONL run store (rewrite `bench/store.rs`)

**Files:** Rewrite `src/core/bench/store.rs`; modify `src/core/config.rs` (`eval_dir()`), `src/error.rs` (reword `BenchRunInvalid` if needed — keep it).

**Interfaces — Produces:**
```rust
pub const SCHEMA_VERSION: u32 = 1;

/// One task's row. Serialized as one JSONL line.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRow {
    pub schema: u32,
    pub run_id: String,
    pub seq: u32,
    pub suite: String,          // "throughput" | "fixture"
    pub task_id: String,        // "depth-16384" | fixture probe id
    pub measure: Measure,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grade: Option<GradeRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Measure {
    pub prompt_n: u64,
    pub decode_samples: Vec<f64>,
    pub prefill_samples: Vec<f64>,
    pub warmup_dropped: u32,    // recorded per §7.4 even though summarize re-derives it
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GradeRow { pub pass: bool, pub reason: Option<String> }

/// A run directory: `<eval>/<run_id>/` with `stamp.json` + `results.jsonl`.
pub struct RunWriter { /* dir, file handle opened O_APPEND, seq counter */ }

impl RunWriter {
    /// Create the dir, write stamp.json (stamp + model name + full launch argv).
    pub fn create(eval_dir: &Path, run_id: &str, head: &RunHead) -> Result<Self, ChekovError>;
    /// Open an existing run for --resume; refuses a differing stamp.
    pub fn resume(eval_dir: &Path, run_id: &str, head: &RunHead) -> Result<(Self, RunLog), ChekovError>;
    /// One row, one O_APPEND write, flushed — a crash loses at most this task.
    pub fn append(&mut self, suite: &str, task_id: &str, body: RowBody) -> Result<(), ChekovError>;
    pub fn dir(&self) -> &Path;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunHead {
    pub model: String,
    pub launch_args: Vec<String>,
    pub stamp: super::stamp::Stamp,
}

pub struct RowBody { pub measure: Measure, pub grade: Option<GradeRow> }

/// A loaded run: head + rows, `completed` = set of (suite, task_id).
pub struct RunLog { pub head: RunHead, pub rows: Vec<TaskRow> }
impl RunLog {
    pub fn load(run_dir: &Path) -> Result<Self, ChekovError>;
    pub fn is_done(&self, suite: &str, task_id: &str) -> bool;
}

pub fn render_run(log: &RunLog) -> String;   // same table as today, from rows
```
- `run_id` = `clock::utc_compact_now()` + `-` + model (caller supplies).
- Config: `pub fn eval_dir(&self) -> PathBuf { self.root.join("eval") }`.
- A malformed jsonl LINE is a loud `BenchRunInvalid` naming the line number, never skipped.

Tests (scratch dirs, as today):
```rust
#[test] fn a_run_round_trips_row_by_row() { /* create → append 2 → load → rows == 2, seq 0,1, stamp equal */ }
#[test] fn resume_reopens_and_skips_completed_tasks() { /* create+append depth-1024 → resume with same head → is_done("throughput","depth-1024"), append more → load shows both */ }
#[test] fn resume_with_a_differing_stamp_is_refused() { /* head with different seed → Err BenchStampMismatch naming "seed" */ }
#[test] fn a_corrupt_line_is_loud_with_its_line_number() { /* write garbage line 2 → load Err contains "line 2" */ }
#[test] fn rendering_recomputes_summaries_from_rows() { /* as today: median from samples, curve refusal, warmup visible */ }
```

Commit: `feat(bench): append-only JSONL run store — a crash loses at most one task`

---

### Task 5: compare over run dirs, refusing on the stamp

**Files:** Rewrite compare paths in `src/core/bench/compare.rs`.

**Interfaces:** `compare_runs(a: &RunLog, b: &RunLog, significance_pct) -> Result<Vec<DepthComparison>, ChekovError>` — first `stamp::first_mismatch` (SKIPPING fields that legitimately differ between candidates under one comparison question: `weights_revision`, `quant`, `corpus_id`? **No** — spec: compare refuses on ANY first differing field; comparing two different models is comparing two stamps that differ on weights_revision…).

**Resolution (matches spec §7.4 intent):** the stamp pins the *environment*; the *candidate* fields (`weights_revision`, `quant`) are what you are comparing. `first_mismatch_env(a, b)` checks every field EXCEPT `weights_revision` and `quant`; those two render in the header instead. `prompt_set_hash`/`corpus_id` MUST match (you cannot compare runs of different task sets). Document this in the fn doc: "the stamp refuses when the environment differs; the model fields are the comparison's subject, not its precondition."

Tests: differing `seed` refused naming "seed"; differing `prompt_set_hash` refused; differing `weights_revision`+`quant` with identical environment compares fine; existing depth-comparison + rendering tests carried over against `RunLog` inputs.

Commit: `feat(bench): compare refuses on the first differing stamp field`

---

### Task 6: pinned sampling + prompt-set hash

**Files:** Modify `src/core/bench/probes.rs`, `src/core/config.rs` (`[bench] seed`, default 42).

- Probe bodies gain `"temperature": 0, "top_k": 1, "seed": <seed>` (translator forwards unknown-to-Anthropic fields? **check**: `to_openai_request` builds the body — if it drops unknown fields, add them post-translation in `runner::cross` by injecting into the forwarded body JSON instead; the test asserts the FORWARDED body carries them, which is the only place it matters).
- `pub fn prompt_set_hash(plan: &SweepPlan, seed: u32) -> String` — sha256 over a canonical string of depths, max_tokens, repetitions, seed, and the probe prompt template text.

Tests: forwarded body carries `temperature:0, top_k:1, seed:42`; hash changes when a depth changes and is stable otherwise.

Commit: `feat(bench): pinned greedy sampling and the prompt-set hash`

---

### Task 7: CLI rewire — bench writes a run dir, `--resume`, compare takes run dirs

**Files:** Modify `src/commands/capability.rs`, `src/error.rs` if a new refusal is needed.

- `Bench` gains `--resume <RUN_ID>`; builds `RunHead` (stamp from: machine_id — `SetupIncomplete` if `None`; engine commit — refuse "unrecorded engine cannot be stamped" (reuse SetupIncomplete naming `chekov update --engine`) **unless** `current_commit` answers; /props ctx + total_slots — extend `runner` with `pub fn read_props(fetch: &PropsFetch) -> Result<PropsInfo{ n_ctx: u32, total_slots: u32 }, _>` and keep `assert_props_ctx` building on it; flag values via `stamp::flag_value` over `server::launch_args`; seed/temp from config; `corpus_id`: `"throughput-v1"` plus `+fixture:<sha256(file)>` when `--fixture`).
- Bench flow: throughput rows appended per depth as measured (suite "throughput", task id `depth-<n>`), fixture rows per probe (suite "fixture"). `--resume` skips completed tasks. Print `render_run` + the run dir path.
- `Compare { a, b }` args become run ids or paths: if the arg is an existing dir → RunLog::load(dir); else try `<eval_dir>/<arg>`.
- Parse test updated for `--resume`.

Manual demonstration: live re-run (server up): `chekov capability bench`, then intentionally `chekov capability bench --resume <id>` to show skip; `capability compare` of two same-stamp runs.

Commit: `feat(bench): run-dir storage wired through the CLI with --resume`

---

### Task 8: CHANGELOG + IDEAS

CHANGELOG (Unreleased/Added + a Changed note that the PR #29 single-file format is replaced before ever appearing in a release). IDEAS: capability status line gains "slice-5 gap part 1 (stamp+JSONL) SHIPPED; parts 2 (lifecycle) and 3 (probe suites) OPEN".

Commit: `docs(bench): changelog and status for the stamp+jsonl store`

## Execution deviations (recorded 2026-08-28, all tasks complete)

- Tasks 4–7 landed as one commit: the store rewrite breaks compare and the
  CLI until all three are updated, and every commit must be green.
- `RowBody` became `Task` (carries suite + task_id) — `append` would otherwise
  exceed the 3-argument gate.
- `assert_props_ctx` now returns `PropsInfo { n_ctx, total_slots }` (the stamp
  needs the slot count); `read_props` is the underlying reader.
- Pinned sampling is injected in `runner::cross` AFTER translation (the
  Anthropic dialect has no `seed` field, and teaching the translator
  bench-only passthrough would change a contract the proxy owns).
- SHA-256's compression uses an indexed register array — clippy's
  `many_single_char_names` bars the FIPS letters.
- `first_mismatch` stays name-only; `mismatch_error` renders the two values
  via `serde_json::to_value` for the refusal text.

## Self-Review
- §7.4 stamp fields: all 17 represented (gguf_sha256→weights_revision deviation flagged; temperature as integer millis to keep field-wise Eq). §7.5: JSONL + stamp.json + resume + foreign-row concerns deferred (no import path exists yet). §4.2 machine id exact. Pinned sampling §7.3.6.
- Env-vs-subject split in compare is an interpretation — flagged in the PR for human review.
- Types consistent: Stamp (T3) → RunHead (T4) → compare (T5) → CLI (T7); PropsFetch reused from shipped runner.
