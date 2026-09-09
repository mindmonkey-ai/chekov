# Foreign-Runtime Timing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Measure throughput through a foreign OpenAI-compatible server with chekov's own wall clock over a streamed response — named on the stamp, loud on everything it cannot derive, byte-for-byte unchanged for llama.cpp runs.

**Architecture:** One new defaulted method on the `HttpClient` seam returns the SSE body plus two measured durations; a pure `timings_from_stream` derives llama.cpp-shaped `Timings` from OpenAI `usage` counts and those durations; `cross_stream_timed` reuses the existing SSE/translator machinery so every downstream consumer (sweep, rows, grading, compare, SVG) is untouched; a `timing_source` stamp field names the source and joins the `--cross-runtime` allow-list; the foreign per-row failure text is fixed everywhere (the PR #56 parked residual).

**Tech Stack:** Rust, ureq (blocking, incremental body read), serde_json, `std::time::Instant`. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-08-31-foreign-timing-design.md` — binding; quoted strings below are copied from it.

## Global Constraints

- `make lint && make test` green before every commit (red-test commits exempt from tests). Suite baseline: 688 lib + 10 integration.
- Functions ≤40 LOC, ≤3 params (bundle structs; bool FIELDS fine, bare bool params not), nesting ≤3; no `unwrap()`/`expect()` outside tests; no new `#[allow]`/`#[expect]`.
- TDD: failing tests committed first (`test(<module>): red — <what>`; compile failure counts as red).
- Pushkin: NEVER shell-read files under `src/` (`config.rs`, `compare.rs`, `store.rs` hard-struck) — Read with offset+limit (≤150 lines) or `mcp__scout__*` only. `tests/**` is WRITE-protected: the integration test `tests/bench_streamed_probe_crosses_the_translator.rs` implements `HttpClient` and constructs `ProbeWire` by literal — nothing this plan adds may force an edit there (that is why the new trait method has a default body).
- Commits end with the two dispatch-supplied trailers.
- llama.cpp-path behaviour, rows, and report output are byte-for-byte unchanged; absent `--runtime` nothing changes.

## File Structure

- `src/core/hub.rs` — `StreamMarks`, the defaulted `post_json_stream_timed` trait method, the `UreqClient` override.
- `src/error.rs` — `ForeignTimingsUnsupported` reshaped to `{ runtime, reason }`, new message.
- `src/core/bench/runner.rs` — `StreamUsage`, `stream_usage`, `timings_from_stream`, `cross_stream_timed`; `cross_fim_chat` rides it; `foreign_timings_error` gains the reason.
- `src/core/bench/stamp.rs` — `timing_source` field 23 + constants; `first_mismatch` 23 entries.
- `src/core/bench/compare.rs` — allow-list grows to 12; mask extended.
- `src/commands/capability.rs` — foreign throughput exec + Streamed row keys; stamp assembly; §7 recast threading for agentic/fixture rows.
- `src/core/bench/store.rs` — `timing source:` header line.
- Docs: README.md, CHANGELOG.md, IDEAS.md.

Execution order: 1, 2, 3, 4, 5.

---

### Task 1: The `timing_source` stamp field and the compare allow-list

**Files:**
- Modify: `src/core/bench/stamp.rs` (field after `runtime`; constants; `first_mismatch`), `src/core/bench/compare.rs` (`CROSS_RUNTIME_ALLOWED`, the cross-runtime mask), every test fixture the compiler flags (add the default line).
- Test: stamp.rs and compare.rs tests modules.

**Interfaces:**
- Consumes: `Stamp.runtime` (exists).
- Produces (Tasks 4-5 consume): `Stamp.timing_source: String`; `pub const TIMING_SERVER: &str = "server-reported";` `pub const TIMING_CHEKOV_STREAMED: &str = "chekov-streamed";`.

- [ ] **Step 1: Failing tests**

stamp.rs (reuse the JSON-literal fixture style of `a_stamp_without_a_runtime_field_reads_as_llama_cpp`):

```rust
#[test]
fn a_stamp_without_a_timing_source_reads_as_server_reported() {
    // same JSON literal as the runtime test, minus both new fields
    // → deserializes; assert stamp.timing_source == TIMING_SERVER
}

#[test]
fn timing_source_differs_after_runtime_and_before_the_engine_commit() {
    // a, b equal; set a.timing_source = TIMING_CHEKOV_STREAMED and
    // a.engine_build_commit = "x" → first_mismatch == Some("timing_source");
    // additionally set a.runtime = "mtplx 1" → Some("runtime")
}
```

compare.rs (extend the existing cross-runtime tests):

```rust
#[test]
fn timing_source_is_allow_listed_only_under_cross_runtime() {
    // b.head.stamp.timing_source = TIMING_CHEKOV_STREAMED:
    // plain compare_runs → Err(BenchStampMismatch { field: "timing_source", .. });
    // with cross_runtime: true → Ok, and cross_runtime_banner names
    // "timing_source: " among its lines.
}
```

Also assert `CROSS_RUNTIME_ALLOWED.len() == 12` inside that test.

- [ ] **Step 2: Watch them fail** (compile error: no field). Commit red: `test(stamp): red — timing_source field, order, allow-list`.
- [ ] **Step 3: Implement.** Field, immediately after `runtime` (spec §6):

```rust
    /// Where the timing numbers came from: the server's own `timings`
    /// object, or chekov's wall clock over a streamed reply (foreign runs).
    #[serde(default = "default_timing_source")]
    pub timing_source: String,
```

`fn default_timing_source() -> String { TIMING_SERVER.to_owned() }` beside `default_runtime`; the two constants; `first_mismatch` becomes 23 entries with `("timing_source", …)` THIRD (after `runtime`). Module doc count 22→23. compare.rs: add `"timing_source"` to `CROSS_RUNTIME_ALLOWED` (now 12) and `b_env.timing_source.clone_from(&a.timing_source)` (or via the existing `mask_cross_runtime`) in the cross-runtime mask. Fix compiler-flagged `Stamp` literals with `timing_source: stamp::TIMING_SERVER.to_owned(),` (production `assemble_stamp` included, hardcoded for now — Task 4 makes it real).
- [ ] **Step 4:** `make lint && make test` green.
- [ ] **Step 5: Commit** — `feat(stamp): timing_source — the stamp names whose clock measured the run`

---

### Task 2: The reshaped error and the timed-stream seam

**Files:**
- Modify: `src/error.rs` (`ForeignTimingsUnsupported`), `src/core/hub.rs` (`StreamMarks`, trait method, `UreqClient` override), `src/core/bench/runner.rs` (`foreign_timings_error` and every raise/match site of the old one-field variant — the compiler enumerates).
- Test: hub.rs and runner.rs tests modules.

**Interfaces:**
- Produces (Task 3 consumes): `pub struct StreamMarks { pub to_first_data: std::time::Duration, pub first_to_done: std::time::Duration }` (hub.rs, `Debug, Clone, Copy`); trait method `fn post_json_stream_timed(&self, req: &JsonRequest) -> Result<(String, StreamMarks), ChekovError>` with the spec §2 default body (refuses with runtime `"unknown"`, reason `"this HTTP client cannot stream-time responses"`); `ChekovError::ForeignTimingsUnsupported { runtime: String, reason: String }` with message exactly (spec §7):

```
the server declared as runtime '{runtime}' gave chekov nothing to time ({reason}) — a foreign run is stream-timed from `usage` token counts, and this reply had none to work with
```

- [ ] **Step 1: Failing tests**

```rust
#[test]
fn the_default_stream_timed_post_refuses_honestly() {
    // a minimal HttpClient impl with only get/post_json → call
    // post_json_stream_timed, assert ForeignTimingsUnsupported whose
    // to_string() contains "cannot stream-time"
}

#[test]
fn the_first_data_scan_fires_on_a_payload_byte_and_not_before() {
    assert!(!super::saw_first_data("event: x\n"));
    assert!(!super::saw_first_data("data:"));
    assert!(super::saw_first_data("data: {"));
    assert!(super::saw_first_data("event: x\ndata: 1\n"));
}
```

(`saw_first_data(buffer: &str) -> bool` is the pure helper the ureq override polls while reading: true once the buffer holds a `data:` line with at least one non-whitespace payload byte. Put it in hub.rs beside the override.)

- [ ] **Step 2: red** (compile). Commit: `test(hub): red — stream-timed seam default and first-data scan`.
- [ ] **Step 3: Implement.** Error reshape + message; update `foreign_timings_error` (runner.rs:523-530) to supply reason `"the reply carried no llama.cpp timings object"` and thread the new field through every constructor site the compiler flags (the Task-4-era tests asserting the old message text update to the new text). Hub: `StreamMarks`, the defaulted trait method (spec §2 body verbatim), and the `UreqClient` override:

```rust
    fn post_json_stream_timed(
        &self,
        req: &JsonRequest,
    ) -> Result<(String, StreamMarks), ChekovError> {
        // Same request shape as post_json; the read is incremental so the
        // first data frame can be timestamped. Thin network I/O — untested
        // by design, like the hub's shard download; the pure parts
        // (saw_first_data, the timing math) carry the tests.
        let started = std::time::Instant::now();
        let response = /* build exactly as post_json does (status-as-error
            off, content-type, bearer) and .send(&req.body), mapping the
            send error to EndpointDown like post_json */;
        /* non-2xx: read the whole body and return the same error post_json
           returns for that status (reuse/extract its status-handling tail
           into a shared helper if that keeps both ≤40 LOC) */
        let mut reader = response.into_body().into_reader();
        let mut buffer = String::new();
        let mut first_data: Option<std::time::Duration> = None;
        let mut chunk = [0_u8; 8192];
        loop {
            let n = /* reader.read(&mut chunk), map err → EndpointDown */;
            if n == 0 { break; }
            buffer.push_str(&String::from_utf8_lossy(&chunk[..n]));
            if first_data.is_none() && saw_first_data(&buffer) {
                first_data = Some(started.elapsed());
            }
        }
        let to_first_data = first_data.ok_or_else(|| ChekovError::EndpointDown {
            url: req.url.clone(),
            reason: "stream ended with no data frame".to_owned(),
        })?;
        Ok((buffer, StreamMarks {
            to_first_data,
            first_to_done: started.elapsed().saturating_sub(to_first_data),
        }))
    }
```

The `/* … */` blocks are instructions: mirror `post_json`'s existing request-building and status-handling code (read it at hub.rs:48-90 first), extracting a shared helper only if function length demands it. `use std::io::Read;` scoped to the impl. Keep the override ≤40 LOC by extracting the read loop into a helper if needed (`read_stream_timed(reader, started) -> Result<(String, StreamMarks), …>` — but that helper then needs the url for errors; a small struct or closure keeps it ≤3 params).
- [ ] **Step 4:** `make lint && make test` green.
- [ ] **Step 5: Commit** — `feat(hub): post_json_stream_timed — the timed streaming seam with an honest default`

---

### Task 3: Derived timings and the timed crossing

**Files:**
- Modify: `src/core/bench/runner.rs` (near `cross_streaming`:266-295 and `stream_timings`:326-333; `cross_fim_chat`:668-682).
- Test: runner.rs tests module.

**Interfaces:**
- Consumes: `StreamMarks`, `post_json_stream_timed` (Task 2); existing `data_lines`, `assemble`, `with_stream_flag`, `forward_of`, `adjust_body`, `Timings`, `ProbeArtifact`, `chat_text_of`, `normalize_chat_fill`.
- Produces (Task 4 consumes): `pub fn cross_stream_timed(wire: &ProbeWire, req: &HttpRequest) -> Result<ProbeArtifact, ChekovError>`. Internal: `struct StreamUsage { prompt_tokens: u64, completion_tokens: u64 }`, `fn stream_usage(sse: &str) -> Option<StreamUsage>` (LAST data frame carrying a `usage` object; read `usage.prompt_tokens`/`usage.completion_tokens`), `fn timings_from_stream(usage: &StreamUsage, marks: &StreamMarks) -> Result<Timings, ChekovError>`.

**Binding math and guards (spec §3)** — `timings_from_stream` returns `Timings { prompt_n: usage.prompt_tokens, prompt_per_second: prompt_n / to_first_data_secs, predicted_n: usage.completion_tokens, predicted_per_second: (predicted_n − 1) / first_to_done_secs, cache_n: 0 }`. Guards fail as `ForeignTimingsUnsupported { runtime: "unknown".into(), reason }` — the caller-facing runtime recast happens where it does today (`foreign_timings_error` sites keep working because the variant matches on shape, not runtime value; verify the recast overwrites the runtime, and make it do so if it doesn't): reasons exactly `"usage.prompt_tokens is 0"`, `"fewer than 2 completion tokens — no decode window to time"`, `"zero-length timing window"`; a missing usage frame fails in `cross_stream_timed` with reason `"no usage object in the stream"`.

- [ ] **Step 1: Failing tests**

```rust
#[test]
fn stream_timings_derive_from_usage_counts_and_the_two_windows() {
    let usage = super::StreamUsage { prompt_tokens: 100, completion_tokens: 51 };
    let marks = crate::core::hub::StreamMarks {
        to_first_data: std::time::Duration::from_millis(500),
        first_to_done: std::time::Duration::from_secs(2),
    };
    let t = super::timings_from_stream(&usage, &marks).unwrap();
    assert_eq!(t.prompt_n, 100);
    assert!((t.prompt_per_second - 200.0).abs() < 1e-9);
    assert_eq!(t.predicted_n, 51);
    assert!((t.predicted_per_second - 25.0).abs() < 1e-9);
    assert_eq!(t.cache_n, 0);
}

#[test]
fn each_underivable_stream_is_refused_with_its_reason() {
    // zero prompt_tokens; completion_tokens 1; zero first_to_done —
    // assert each error text contains its spec reason string.
}

#[test]
fn the_usage_frame_is_the_last_one_and_absence_is_none() {
    let sse = "data: {\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":2}}\n\
               data: {\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":8}}\n\
               data: [DONE]\n";
    let u = super::stream_usage(sse).unwrap();
    assert_eq!((u.prompt_tokens, u.completion_tokens), (9, 8));
    assert!(super::stream_usage("data: {\"x\":1}\n").is_none());
}

#[test]
fn cross_stream_timed_assembles_the_body_and_times_it_with_chekovs_clock() {
    // A canned HttpClient overriding post_json_stream_timed with a real
    // Anthropic-translatable SSE body (copy the SSE fixture style from the
    // existing cross_streaming tests / the streamed-probe fixtures) whose
    // terminal frame carries usage {prompt_tokens: 10, completion_tokens: 3},
    // plus marks (100ms, 1s). Assert: the request body sent carried
    // "stream":true; the artifact's anthropic_body contains the streamed
    // text; timings == timings_from_stream of those inputs (decode 2.0).
    // And: the same body with no usage frame fails with reason
    // "no usage object in the stream".
}
```

- [ ] **Step 2: red** (compile). Commit: `test(runner): red — stream-derived timings and the timed crossing`.
- [ ] **Step 3: Implement.** `cross_stream_timed` mirrors `cross_streaming` (runner.rs:276-295) line for line — same `forward_of`/`with_stream_flag`/`adjust_body` prelude, same `data_lines` → `stream_translator` → `assemble` — with two substitutions: the POST goes through `wire.http.post_json_stream_timed(...)` capturing `(sse, marks)`, and the timings come from `stream_usage(&sse)` + `timings_from_stream` instead of `stream_timings`. Doc comment states the honesty caveats (wire overhead included; first-frame ≈ end-of-prefill because these servers stream tokens as generated — spec §1). Then flip ONE line in `cross_fim_chat` (runner.rs:676): `cross(wire, …)` → `cross_stream_timed(wire, …)` — spec §5 pins the chat arm to the timed path (it is selected exactly on foreign runs); its existing tests updated to canned-stream fakes where they canned `post_json` (keep the pins assertions).
- [ ] **Step 4:** `make lint && make test` green.
- [ ] **Step 5: Commit** — `feat(runner): cross_stream_timed — chekov-clock timings from usage over the existing SSE machinery`

---

### Task 4: Wiring — foreign throughput, the stamp, the report line, the row text

**Files:**
- Modify: `src/commands/capability.rs` — `run_throughput` (:1949-1980), `run_suites`/`SuiteInputs`, the stamp-assembly helpers Task 4 of the previous plan created (`foreign_identity`/`local_identity`/`StampParts` area, ~:2280-2420 — outline first), `run_agentic`/`run_fixture`/`append_probe` (§7 recast), `AgenticPass`.
- Modify: `src/core/bench/store.rs` — the header/render area beside `fim_transport_line` (~:906).
- Test: capability.rs and store.rs tests modules.

**Interfaces:**
- Consumes: `cross_stream_timed` (T3), `TIMING_SERVER`/`TIMING_CHEKOV_STREAMED` (T1), the existing `SuiteInputs.fim`, `foreign_timings_error`, `RuntimeSpec.stored()`.
- Produces: foreign runs whose throughput rows are stream-timed and stamped `chekov-streamed`.

**Binding decisions (spec §5, §6, §7):**
- `run_throughput` on a foreign run crosses via `|req| runner::cross_stream_timed(wire, req)` and records the row with `transport: store::Transport::Streamed` AND queries resume-doneness with the matching streamed key — a foreign resume must not re-run recorded depths (`TaskKey` carries transport; use the streamed constructor or build the key with `Transport::Streamed`). llama.cpp rows stay `Buffered` with today's key, byte-for-byte.
- Threading: extend `SuiteInputs` (which already carries `fim`) with the timing choice — one field, e.g. `timing: &'a str` (a `TIMING_*` constant) or a small enum; `run_throughput` stays ≤3 params by bundling (e.g. a `ThroughputPass { plan, wire, timed: bool }` bundle, or passing the exec closure in — implementer's choice, gates hold).
- Stamp: the foreign identity path stamps `timing_source: TIMING_CHEKOV_STREAMED`; the local path `TIMING_SERVER` (replacing Task 1's hardcode).
- Report (spec §6): in store.rs beside `fim_transport_line`, a header line printed only when `stamp.timing_source != TIMING_SERVER`, exactly: `timing source: chekov-streamed (client wall-clock over SSE; includes wire overhead)`. llama.cpp render output byte-identical (assert an existing rendered fixture contains no `timing source:`).
- §7: on a foreign run, agentic and fixture per-row failures append through the runtime-aware recast — wrap the crossing `Result` with `foreign_timings_error` (carrying the run's runtime) BEFORE `append_probe`/`failed_probe` turns it into a row reason. Thread the runtime into `AgenticPass` and `run_fixture` the same way `fim` travels. llama.cpp rows keep today's text exactly.

- [ ] **Step 1: Failing tests** (shape against the helpers you extract; the asserted FACTS are binding)

```rust
#[test]
fn a_foreign_throughput_row_is_streamed_and_resume_sees_it() {
    // the exec/key selection helper: foreign → (timed crossing,
    // Transport::Streamed key); local → (cross, Buffered key).
}

#[test]
fn the_stamp_names_chekovs_clock_exactly_on_foreign_runs() {
    // foreign identity → timing_source == TIMING_CHEKOV_STREAMED;
    // local → TIMING_SERVER.
}

#[test]
fn a_foreign_agentic_row_failure_names_the_runtime_not_the_engine() {
    // run the recast the agentic/fixture path applies to a BenchNoTimings
    // error under runtime Some("mtplx 0.4.1"): the resulting row-reason
    // string contains "mtplx 0.4.1" and not "chekov update --engine";
    // under None it is BenchNoTimings' text unchanged.
}
```

store.rs test: `the_header_names_chekovs_clock_only_when_it_measured` — rendered run with `TIMING_CHEKOV_STREAMED` contains the exact §6 line; with `TIMING_SERVER` contains no `timing source:`.

- [ ] **Step 2: red.** Commit: `test(capability): red — foreign timed throughput, stamp source, row text`.
- [ ] **Step 3: Implement** per the binding decisions. Verify with the compiler that no `ProbeWire` or `tests/**` change is forced.
- [ ] **Step 4:** `make lint && make test` green; `cargo run --quiet -- capability bench --dry-run` still prints today's plan.
- [ ] **Step 5: Commit** — `feat(capability): foreign runs are stream-timed — streamed rows, chekov-streamed stamp, runtime-aware row text`

---

### Task 5: Docs

**Files:** README.md, CHANGELOG.md, IDEAS.md.

- [ ] **Step 1: Write.** README: replace the PR #56 "requires llama.cpp-style timings" qualification with the shipped truth — foreign runs are stream-timed from `usage` token counts by chekov's own clock (wire overhead included, said plainly); the `timing source:` report line; `--cross-runtime` now also permits `timing_source`. CHANGELOG unreleased entry: the stamp field (default `server-reported`, stored runs unaffected), the seam method, the derivation, the report line, the fixed foreign row text. IDEAS: update the foreign-runtime entry's follow-up list — timing design SHIPPED (say the two-window honesty limit), remaining: foreign agentic/fixture timing (rides the same mechanism), live MLX verification (approval-gated). No claim of a measured foreign result anywhere.
- [ ] **Step 2:** `make lint && make test` (fence tests).
- [ ] **Step 3: Commit** — `docs: foreign timing — chekov-streamed measurement documented, follow-ups updated`

---

## Self-Review (performed while writing)

- **Spec coverage:** §2 seam → T2; §3 math/guards → T3; §4 crossing → T3; §5 selection (throughput T4, chat-FIM T3, agentic/fixture excluded-but-retexted T4); §6 stamp/report/compare → T1+T4; §7 residual + error reshape → T2+T4; §8 → T2; §9 distributed; §10 respected (no TTFT metric, no foreign tune, llama.cpp untouched).
- **Placeholder scan:** T2's `/* … */` blocks are adapt-to-code instructions mirroring named existing code (post_json's tail), with binding behaviour stated — deliberate, as in the two prior plans.
- **Type consistency:** `StreamMarks` fields used identically in T2 tests and T3 math; `TIMING_*` constants defined T1, consumed T4; `cross_stream_timed(wire, req)` produced T3, consumed T4; guard reason strings identical in spec, T3 interfaces, and T3 tests.
