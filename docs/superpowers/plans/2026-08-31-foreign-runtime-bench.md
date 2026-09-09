# Foreign-Runtime Bench Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a foreign OpenAI-compatible server (MTPLX, MLX) a first-class bench candidate — declared runtime identity in the stamp, a chat-completions FIM fallback for the codebase suite, and an explicit `compare --cross-runtime` mode.

**Architecture:** A new `runtime` stamp field (serde-defaulted to `llama.cpp`) makes cross-runtime pairs mismatch on a named field first; a `RuntimeSpec` parsed from `--runtime name@version` drives a `UseRunning`-only bench path with `/v1/models` readiness and `unmanaged` flag sentinels; `cross_infill` gains a chat-transport sibling selected by runtime; `compare --cross-runtime` extends the existing subject-field masking by an exact allow-list and prints a banner.

**Tech Stack:** Rust, clap, serde/serde_json, thiserror, existing `HttpClient` seam and Anthropic↔OpenAI translator. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-08-31-foreign-runtime-bench-design.md` — the binding authority; every quoted string below is copied from it.

## Global Constraints

- `make lint && make test` green before every commit (`cargo fmt --check && cargo clippy --all-targets -- -D warnings`; `cargo test --locked`). Red-test commits may fail tests (or not compile) but are committed as red on purpose.
- Functions ≤40 LOC, ≤3 params (bundle structs), nesting ≤3, no boolean flag params (a `bool` field on a bundle struct is fine; a bare `bool` parameter is not).
- No `unwrap()`/`expect()` outside `#[cfg(test)]`; `#![forbid(unsafe_code)]`; no new `#[allow]`/`#[expect]` anywhere.
- TDD: commit the failing test first (`test(<module>): red — <what>`; a compile failure counts as red), then the implementation.
- Pushkin read gate: NEVER shell-read (cat/grep/sed/head/tail/git diff/git show) any file under `src/`. Inspect source ONLY via the Read tool with offset+limit (≤150 lines) or `mcp__scout__*` tools. Never `cd` in Bash; run from the repo root.
- Every commit message ends with the two trailers the dispatch supplies (Co-Authored-By + Claude-Session).
- Absent `--runtime`, every current behaviour is byte-for-byte unchanged — every task must keep the existing 667+ tests green apart from fixtures the stamp field forces to update.

## File Structure

- `src/core/bench/stamp.rs` — gains `runtime` field, `RUNTIME_LLAMA_CPP`, 22-field `first_mismatch`.
- `src/error.rs` — neutral `BenchStampMismatch` message; new `RuntimeFlagInvalid`, `RuntimeNeedsRunningServer`.
- `src/core/bench/runtime.rs` (new) — `RuntimeSpec` (parse/stored), `foreign_ready`.
- `src/core/bench/runner.rs` — `FimTransport`, `ProbeWire.fim`, `chat_fim_prompt`, `cross_fim_chat`, `normalize_chat_fill`, `chat_fim_hash`.
- `src/core/bench/codebase/run.rs` — transport selection at the `cross_infill` call site.
- `src/core/bench/store.rs` — `fim transport: …` line in the codebase render.
- `src/commands/capability.rs` — `--runtime`/`--upstream` flags, foreign candidate path, foreign stamp parts, `--cross-runtime` on compare.
- `src/core/bench/compare.rs` — `CompareOpts`, extended masking, `cross_runtime_banner`.
- Docs: README.md, CHANGELOG.md, IDEAS.md.

Execution order: 1, 2, 3, 4, 5, 6 (each task builds on the previous).

---

### Task 1: The `runtime` stamp field and the neutral mismatch message

**Files:**
- Modify: `src/core/bench/stamp.rs:15-17` (field), `:90-126` (`first_mismatch`), `:1` (doc "21-field" → "22-field")
- Modify: `src/error.rs:209-216` (`BenchStampMismatch` message)
- Modify: every test fixture that constructs `Stamp { … }` literally (compile errors will enumerate them; known sites: `src/commands/capability.rs` test helpers `eligible_stamp` ~:2638, `src/core/bench/store.rs` tests, `src/core/bench/compare.rs` tests) — add `runtime: stamp::RUNTIME_LLAMA_CPP.to_owned(),`
- Test: `src/core/bench/stamp.rs` tests module

**Interfaces:**
- Consumes: nothing new.
- Produces: `pub const RUNTIME_LLAMA_CPP: &str = "llama.cpp";` and `Stamp.runtime: String` — Tasks 3, 4, 5 read both.

- [ ] **Step 1: Write the failing tests** (in stamp.rs's existing `#[cfg(test)]` module; copy an existing stamp fixture helper if one exists there, else build via serde as below)

```rust
#[test]
fn a_stamp_without_a_runtime_field_reads_as_llama_cpp() {
    // Every run stored before this field existed was a llama.cpp run.
    let json = serde_json::json!({
        "machine_id": "m", "engine_build_commit": "c",
        "weights_revision": "r/s", "quant": "q", "ctx": 1, "n_parallel": 1,
        "kv_unified": "engine-default", "n_batch": "engine-default",
        "n_ubatch": "engine-default", "type_k": "engine-default",
        "type_v": "engine-default", "flash_attn": "engine-default",
        "seed": 0, "temperature_milli": 0, "chekov_version": "0",
        "prompt_set_hash": "h", "corpus_id": "corp"
    });
    let stamp: super::Stamp = serde_json::from_value(json).unwrap();
    assert_eq!(stamp.runtime, super::RUNTIME_LLAMA_CPP);
}

#[test]
fn runtime_differs_before_the_engine_commit() {
    let mut a: super::Stamp =
        serde_json::from_value(serde_json::json!({
            "machine_id": "m", "engine_build_commit": "aaa",
            "weights_revision": "r/s", "quant": "q", "ctx": 1, "n_parallel": 1,
            "kv_unified": "engine-default", "n_batch": "engine-default",
            "n_ubatch": "engine-default", "type_k": "engine-default",
            "type_v": "engine-default", "flash_attn": "engine-default",
            "seed": 0, "temperature_milli": 0, "chekov_version": "0",
            "prompt_set_hash": "h", "corpus_id": "corp"
        }))
        .unwrap();
    let mut b = a.clone();
    a.runtime = "mtplx 0.4.1".to_owned();
    a.engine_build_commit = "0.4.1".to_owned();
    b.engine_build_commit = "bbb".to_owned();
    assert_eq!(super::first_mismatch(&a, &b), Some("runtime"));
}
```

- [ ] **Step 2: Run and watch them fail**

Run: `cargo test --locked a_stamp_without_a_runtime_field_reads_as_llama_cpp runtime_differs`
Expected: compile failure — `Stamp` has no field `runtime`, no `RUNTIME_LLAMA_CPP`. Commit as red: `test(stamp): red — runtime field, default and mismatch order`.

- [ ] **Step 3: Implement**

In `src/core/bench/stamp.rs`, immediately after `machine_id` (spec §3 — BEFORE `engine_build_commit`):

```rust
    /// The runtime serving the model: `llama.cpp` for every run chekov
    /// launches; the declared `<name> <version>` for a foreign server.
    /// Stored runs from before this field existed were all llama.cpp,
    /// which is what the serde default says.
    #[serde(default = "default_runtime")]
    pub runtime: String,
```

Near `exec_target_off`:

```rust
/// A stamp written before the runtime field existed came from llama.cpp.
fn default_runtime() -> String {
    RUNTIME_LLAMA_CPP.to_owned()
}

/// `Stamp.runtime` for every run chekov launches itself.
pub const RUNTIME_LLAMA_CPP: &str = "llama.cpp";
```

In `first_mismatch`, the array becomes `[(&'static str, bool); 22]` and gains, as the SECOND entry (after `machine_id`, before `engine_build_commit`):

```rust
        ("runtime", a.runtime != b.runtime),
```

Update the module doc (`//! The 21-field configuration stamp` → 22) and the `Stamp` struct doc if it counts fields. The llama.cpp float-associativity paragraph at stamp.rs:3-6 stays — it is the right home for it (spec §3).

In `src/error.rs:209-215`, replace the `BenchStampMismatch` message with exactly (spec §3):

```rust
    #[error(
        "bench stamp mismatch on '{field}' ({a} vs {b}) — results are \
         comparable only inside one pinned configuration (runtime, build, \
         flags and sampling all held constant); re-bench under a matching \
         stamp and compare those runs"
    )]
    BenchStampMismatch { field: String, a: String, b: String },
```

Fix every `Stamp { … }` literal the compiler now rejects by adding `runtime: crate::core::bench::stamp::RUNTIME_LLAMA_CPP.to_owned(),` (tests only — production construction is `assemble_stamp`, updated in Task 4; until then give it the same `RUNTIME_LLAMA_CPP` line so it compiles). If any test asserts the old mismatch message text, update it to the new text.

- [ ] **Step 4: Run the full suite** — `make lint && make test`, all green.
- [ ] **Step 5: Commit** — `feat(stamp): the runtime field — cross-runtime pairs mismatch on a named field first`

---

### Task 2: `RuntimeSpec`, the two errors, and foreign readiness

**Files:**
- Create: `src/core/bench/runtime.rs`
- Modify: `src/core/bench/mod.rs` (add `pub mod runtime;` beside the existing module list)
- Modify: `src/error.rs` (two new variants, near `BenchStampMismatch`)
- Test: inline `#[cfg(test)]` in `src/core/bench/runtime.rs`

**Interfaces:**
- Consumes: `crate::core::hub::HttpClient` (`get(&self, url: &str) -> Result<String, ChekovError>`), `ChekovError::EndpointDown { url, reason }`.
- Produces (Task 4 consumes all of these):
  - `pub struct RuntimeSpec { pub name: String, pub version: String }`
  - `impl RuntimeSpec { pub fn parse(value: &str) -> Result<Self, ChekovError>; #[must_use] pub fn stored(&self) -> String }`
  - `pub fn foreign_ready(http: &dyn HttpClient, base_url: &str) -> Result<Vec<String>, ChekovError>`

- [ ] **Step 1: Write the failing tests** (new file with tests; module doc first)

```rust
#[cfg(test)]
mod tests {
    use super::RuntimeSpec;
    use crate::core::hub::{HttpClient, JsonRequest};
    use crate::error::ChekovError;

    #[test]
    fn a_runtime_spec_parses_name_at_version_and_stores_with_a_space() {
        let spec = RuntimeSpec::parse("mtplx@0.4.1").unwrap();
        assert_eq!(spec.name, "mtplx");
        assert_eq!(spec.version, "0.4.1");
        assert_eq!(spec.stored(), "mtplx 0.4.1");
    }

    #[test]
    fn the_last_at_sign_splits_so_a_version_may_not_carry_one() {
        let spec = RuntimeSpec::parse("mlx-lm@v0.2@rc1").unwrap();
        assert_eq!(spec.name, "mlx-lm@v0.2");
        // …and that name is refused for the '@' it now carries:
        assert!(RuntimeSpec::parse("mlx lm@1").is_err());
        let _ = spec;
    }

    #[test]
    fn each_malformed_spelling_is_refused_with_its_reason() {
        for (value, needle) in [
            ("mtplx", "missing '@'"),
            ("@0.4.1", "empty name"),
            ("MTPLX@1", "lowercase"),
            ("m tplx@1", "lowercase"),
            ("mtplx@", "empty version"),
            ("mtplx@0 4", "whitespace"),
        ] {
            let err = RuntimeSpec::parse(value).unwrap_err();
            let text = err.to_string();
            assert!(
                text.contains(needle) && text.contains(value),
                "{value}: {text}"
            );
        }
    }

    struct CannedModels(&'static str);
    impl HttpClient for CannedModels {
        fn get(&self, _url: &str) -> Result<String, ChekovError> {
            Ok(self.0.to_owned())
        }
        fn post_json(&self, _req: &JsonRequest) -> Result<String, ChekovError> {
            unreachable!("readiness never POSTs")
        }
    }

    #[test]
    fn foreign_readiness_lists_the_served_ids() {
        let http = CannedModels(r#"{"object":"list","data":[{"id":"a"},{"id":"b"}]}"#);
        let ids = super::foreign_ready(&http, "http://h:1").unwrap();
        assert_eq!(ids, vec!["a".to_owned(), "b".to_owned()]);
    }

    #[test]
    fn an_empty_list_is_ready_and_a_shapeless_reply_is_not() {
        let empty = CannedModels(r#"{"data":[]}"#);
        assert_eq!(super::foreign_ready(&empty, "http://h:1").unwrap(), Vec::<String>::new());
        let shapeless = CannedModels("not json");
        let err = super::foreign_ready(&shapeless, "http://h:1").unwrap_err();
        assert!(matches!(err, ChekovError::EndpointDown { .. }));
    }
}
```

Note the second test's first assertion documents WHY last-`@` splitting is safe: a name that swallowed an extra `@` fails the name charset check. `mlx-lm@v0.2` contains `@` which `[a-z0-9._-]` refuses — so `RuntimeSpec::parse("mlx-lm@v0.2@rc1")` must actually be an `Err`. Correct the test to assert that directly:

```rust
    #[test]
    fn the_last_at_sign_splits_so_an_at_in_the_name_is_refused() {
        let err = RuntimeSpec::parse("mlx-lm@v0.2@rc1").unwrap_err();
        assert!(err.to_string().contains("lowercase"));
    }
```

(Use this corrected version, not the first draft above.)

- [ ] **Step 2: Watch them fail** — compile failure (no module). Commit red: `test(runtime): red — spec parse, stored form, foreign readiness`.

- [ ] **Step 3: Implement.** In `src/error.rs`, after `BenchStampMismatch` (messages verbatim from spec §8):

```rust
    #[error("--runtime '{value}' is not <name>@<version> — {reason}")]
    RuntimeFlagInvalid { value: String, reason: String },

    #[error(
        "--runtime {runtime} benches a server you started — chekov cannot \
         launch a {runtime} server; start it, then re-run (the subject must \
         already be serving)"
    )]
    RuntimeNeedsRunningServer { runtime: String },
```

`src/core/bench/runtime.rs`:

```rust
//! A declared foreign runtime (spec 2026-08-31 §2, §4): parsed from
//! `--runtime <name>@<version>`, stored on the stamp as `<name> <version>`,
//! and made ready by listing `/v1/models` — chekov never launches, installs,
//! or probes a foreign server's identity; it prints what was declared and
//! what is served, and measures.

use serde_json::Value;

use crate::core::hub::HttpClient;
use crate::error::ChekovError;

/// One declared runtime. `@` is a CLI spelling; `stored` is the stamp's.
pub struct RuntimeSpec {
    pub name: String,
    pub version: String,
}

impl RuntimeSpec {
    /// Split on the LAST `@`; name `[a-z0-9][a-z0-9._-]*`, version non-empty
    /// with no whitespace. Every refusal names the value and the reason.
    pub fn parse(value: &str) -> Result<Self, ChekovError> {
        let refuse = |reason: &str| ChekovError::RuntimeFlagInvalid {
            value: value.to_owned(),
            reason: reason.to_owned(),
        };
        let (name, version) = value.rsplit_once('@').ok_or_else(|| refuse("missing '@'"))?;
        if name.is_empty() {
            return Err(refuse("empty name"));
        }
        if !name_ok(name) {
            return Err(refuse("name must be lowercase [a-z0-9._-]"));
        }
        if version.is_empty() {
            return Err(refuse("empty version"));
        }
        if version.chars().any(char::is_whitespace) {
            return Err(refuse("version contains whitespace"));
        }
        Ok(Self {
            name: name.to_owned(),
            version: version.to_owned(),
        })
    }

    /// The stamp's spelling: `<name> <version>`.
    #[must_use]
    pub fn stored(&self) -> String {
        format!("{} {}", self.name, self.version)
    }
}

fn name_ok(name: &str) -> bool {
    let mut chars = name.chars();
    let first_fits = chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    first_fits
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || ".-_".contains(c))
}

/// Foreign readiness (spec §4): one plain `GET /v1/models`; a 200 with a
/// `data` array is ready and its `id`s are returned FOR PRINTING — chekov
/// cannot know how a foreign server names the weights, so it reports and
/// lets the human read. Anything else is `EndpointDown`.
pub fn foreign_ready(http: &dyn HttpClient, base_url: &str) -> Result<Vec<String>, ChekovError> {
    let url = format!("{base_url}/v1/models");
    let body = http.get(&url)?;
    let parsed: Value = serde_json::from_str(&body).map_err(|_| ChekovError::EndpointDown {
        url: url.clone(),
        reason: "/v1/models did not return JSON".to_owned(),
    })?;
    let data = parsed
        .get("data")
        .and_then(Value::as_array)
        .ok_or(ChekovError::EndpointDown {
            url,
            reason: "/v1/models reply has no `data` array".to_owned(),
        })?;
    Ok(data
        .iter()
        .filter_map(|m| m.get("id").and_then(Value::as_str))
        .map(str::to_owned)
        .collect())
}
```

(If clippy asks for `ok_or_else` on the non-trivial `EndpointDown`, comply; keep `url.clone()` only where two arms need it.)

- [ ] **Step 4: Run** — `make lint && make test`, green.
- [ ] **Step 5: Commit** — `feat(runtime): RuntimeSpec parse/stored and /v1/models foreign readiness`

---

### Task 3: The chat-completions FIM transport

**Files:**
- Modify: `src/core/bench/runner.rs` (near `cross_infill` at :537 and `infill_body` at :581; `ProbeWire` definition — locate with `mcp__scout__go_to_definition` on `ProbeWire`)
- Modify: `src/core/bench/codebase/run.rs` (the `cross_infill` call site — locate with `mcp__scout__find_references` on `cross_infill`)
- Modify: `src/commands/capability.rs:1491-1524` (`run_suites` builds `ProbeWire`) and `judge_wire` :1321 (second `ProbeWire` constructor) — both gain `fim:` (Task 4 flips the selection; here both pass `FimTransport::Infill`)
- Modify: `src/core/bench/store.rs` — the codebase section renderer (locate `render_codebase`) gains one header line
- Test: `src/core/bench/runner.rs` tests module

**Interfaces:**
- Consumes: `InfillTask`, `InfillOutcome`, `ProbeArtifact`, `n_predict_for`, `timings_from`, `probes::anthropic_post` — all existing in runner.rs/probes.rs; `stamp::RUNTIME_LLAMA_CPP` (Task 1).
- Produces (Task 4 consumes): `pub enum FimTransport { Infill, Chat }` (derive `Clone, Copy, PartialEq, Eq, Debug`); `ProbeWire.fim: FimTransport`; `pub fn cross_fim(wire: &ProbeWire, task: &InfillTask) -> Result<InfillOutcome, ChekovError>` (dispatches on `wire.fim`); `pub fn chat_fim_hash(base: &str) -> String`; `pub const FIM_CHAT_INSTRUCTION: &str`.

- [ ] **Step 1: Write the failing tests** (runner.rs tests module; a canned `HttpClient` that records the posted body already has siblings there — follow their shape)

```rust
#[test]
fn the_chat_fim_prompt_carries_the_instruction_the_extra_and_the_three_sections() {
    let task = super::InfillTask {
        prefix: "fn a() {",
        suffix: "}",
        gold_lines: 1,
        extra: Some(super::ExtraChunk { filename: "lib.rs", text: "pub fn b() {}" }),
    };
    let prompt = super::chat_fim_prompt(&task);
    assert!(prompt.starts_with(super::FIM_CHAT_INSTRUCTION));
    for needle in ["FILE lib.rs:", "pub fn b() {}", "PREFIX:\nfn a() {", "SUFFIX:\n}", "MIDDLE:\n"] {
        assert!(prompt.contains(needle), "missing {needle}");
    }
    let suffix_at = prompt.find("SUFFIX:").unwrap();
    assert!(prompt.find("PREFIX:").unwrap() < suffix_at);
    assert!(suffix_at < prompt.find("MIDDLE:").unwrap());
}

#[test]
fn a_fenced_reply_is_unwrapped_and_one_trailing_newline_trimmed() {
    assert_eq!(super::normalize_chat_fill("```rust\nlet x = 1;\n```\n"), "let x = 1;");
    assert_eq!(super::normalize_chat_fill("let x = 1;\n"), "let x = 1;");
    // A fence in the middle is content, not wrapping:
    assert_eq!(super::normalize_chat_fill("a\n```\nb"), "a\n```\nb");
}

#[test]
fn the_chat_fim_hash_diverges_from_its_base_and_is_stable() {
    let a = super::chat_fim_hash("codebase-only");
    assert_ne!(a, "codebase-only");
    assert_eq!(a, super::chat_fim_hash("codebase-only"));
    assert_ne!(a, super::chat_fim_hash("other"));
    assert_eq!(a.len(), 12);
}
```

- [ ] **Step 2: Watch them fail** — compile failure. Commit red: `test(runner): red — chat-FIM prompt, normalization, hash`.

- [ ] **Step 3: Implement.** In runner.rs, beside the infill items:

```rust
/// Which wire fills a codebase mask: llama.cpp's native `/infill`, or a
/// deterministic chat-completions instruction for a runtime that has no
/// FIM endpoint (spec §6). The transport is a function of the runtime, so
/// the report derives it from the stamp instead of storing it twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FimTransport {
    Infill,
    Chat,
}

/// The chat arm's fixed instruction (spec §6, verbatim). Hashing it into the
/// prompt-set hash makes a template edit a NAMED stamp change.
pub const FIM_CHAT_INSTRUCTION: &str = "You are completing code. Output ONLY \
the missing code between PREFIX and SUFFIX. No explanation, no code fences, \
no repetition of the prefix or suffix.\n";

/// The one user message the chat arm sends: instruction, the extra file when
/// this arm carries one, then PREFIX/SUFFIX/MIDDLE.
fn chat_fim_prompt(task: &InfillTask) -> String {
    let extra = task.extra.as_ref().map_or_else(String::new, |e| {
        format!("FILE {}:\n{}\n\n", e.filename, e.text)
    });
    format!(
        "{FIM_CHAT_INSTRUCTION}\n{extra}PREFIX:\n{}\n\nSUFFIX:\n{}\n\nMIDDLE:\n",
        task.prefix, task.suffix
    )
}

/// Spec §6's two normalization rules, in order, and nothing else.
fn normalize_chat_fill(reply: &str) -> String {
    let unfenced = strip_whole_fence(reply);
    unfenced.strip_suffix('\n').unwrap_or(&unfenced).to_owned()
}

/// Rule 1: when the ENTIRE reply is one fenced block, strip the fences and
/// any language tag. A fence anywhere else is content.
fn strip_whole_fence(reply: &str) -> String {
    let trimmed = reply.trim_end_matches('\n');
    let Some(rest) = trimmed.strip_prefix("```") else {
        return reply.to_owned();
    };
    let Some(body) = rest.strip_suffix("```") else {
        return reply.to_owned();
    };
    // Drop the language tag line (possibly empty) after the opening fence.
    match body.split_once('\n') {
        Some((_tag, inner)) => inner.to_owned(),
        None => String::new(),
    }
}

/// The stamp's prompt-set hash when the chat arm filled the codebase suite:
/// hash(existing value ‖ the template), first twelve hex chars (spec §6).
#[must_use]
pub fn chat_fim_hash(base: &str) -> String {
    let canonical = format!("{base}|chat-fim|{FIM_CHAT_INSTRUCTION}");
    crate::core::hash::sha256_hex(canonical.as_bytes())[..12].to_owned()
}

/// The chat-completions fill: same pins as `/infill` (`temperature 0`,
/// `top_k 1`, the gold-bounded budget), one deterministic user message,
/// crossing the translator exactly as the agentic probes do.
fn cross_fim_chat(wire: &ProbeWire, task: &InfillTask) -> Result<InfillOutcome, ChekovError> {
    let body = serde_json::json!({
        "model": "claude-sonnet-4",
        "max_tokens": n_predict_for(task.gold_lines),
        "temperature": 0,
        "top_k": 1,
        "messages": [{"role": "user", "content": chat_fim_prompt(task)}],
    });
    let artifact = /* send via the same path the agentic probes use — the
        facade/translator crossing that run_tool_case's helper performs; reuse
        the existing probe-sending helper in this file (the one every
        run_*_case funnels through) with probes::anthropic_post(&body) */
        send_probe(wire, &crate::core::bench::probes::anthropic_post(&body))?;
    let text = /* extract content[0].text from artifact.anthropic_body via
        the existing extraction helper used by the graders; if none exists in
        runner.rs, parse: serde_json::from_str::<Value>(&artifact.anthropic_body)
        .ok().and_then(|v| v["content"][0]["text"].as_str().map(str::to_owned)) */
        chat_text_of(&artifact)?;
    Ok(InfillOutcome::Answered(ProbeArtifact {
        anthropic_body: normalize_chat_fill(&text),
        timings: artifact.timings,
    }))
}

/// One fill, whichever wire this run rides (spec §6).
pub fn cross_fim(wire: &ProbeWire, task: &InfillTask) -> Result<InfillOutcome, ChekovError> {
    match wire.fim {
        FimTransport::Infill => cross_infill(wire, task),
        FimTransport::Chat => cross_fim_chat(wire, task),
    }
}
```

The two `/* … */` blocks above are instructions, not code to paste: runner.rs already has the probe-send helper every agentic case funnels through (find it via `mcp__scout__find_references` on `anthropic_post`) and may already extract reply text; reuse those, matching their actual names and signatures. `chat_text_of` failing to find text is a broken reply, not an empty answer — return `ChekovError::ProxyBadRequest { reason: "chat fill has no text content".to_owned() }` exactly as `cross_infill` does for missing `content` (runner.rs:557-562).

Add `pub fim: FimTransport` to `ProbeWire`; update its two constructors (capability.rs:1496 `run_suites`, :1321 `judge_wire`) to `fim: runner::FimTransport::Infill` — Task 4 makes `run_suites` select by runtime. In `src/core/bench/codebase/run.rs`, replace the `cross_infill(` call with `cross_fim(` (same arguments — the wire now carries the choice).

In `src/core/bench/store.rs`'s codebase section renderer (`render_codebase`), print one header line derived from the stamp, before the tier rows:

```rust
    // spec §6: the report names the wire that filled the rows.
    let transport = if log.head.stamp.runtime == crate::core::bench::stamp::RUNTIME_LLAMA_CPP {
        "/infill"
    } else {
        "chat"
    };
    // → push the line: format!("fim transport: {transport}")
```

(Adapt to how that renderer builds lines — read it first; if `render_codebase` lacks access to the head, thread `&log.head.stamp.runtime` in from its caller rather than widening the signature past 3 params — a small `&RunHead` or `&Stamp` reference param replacing none is fine if it stays ≤3.) Add/extend a store render test asserting a rendered codebase run contains `fim transport: /infill` for a `RUNTIME_LLAMA_CPP` stamp and `fim transport: chat` otherwise.

- [ ] **Step 4: Run** — `make lint && make test`, green (codebase suite still rides `/infill` everywhere — behaviour unchanged).
- [ ] **Step 5: Commit** — `feat(runner): chat-completions FIM transport behind FimTransport, selected by the wire`

---

### Task 4: Bench wiring — `--runtime`, `--upstream`, the foreign candidate path

**Files:**
- Modify: `src/commands/capability.rs` — `BenchOpts` :100-143, `BenchArgs` :834-860, `resolve_candidates` :724-758, `run_candidate` :1179-1204, `measure_candidate` :1231-1265, `run_suites` :1491-1524, `HeadInputs`/`build_head`/`assemble_stamp`/`stamped_flags` :1912-2089, `head_corpus` :1962-1976, dry-run/plan lines as needed
- Test: capability.rs tests module (parse tests live at :2215+)

**Interfaces:**
- Consumes: `RuntimeSpec`, `foreign_ready` (Task 2); `RUNTIME_LLAMA_CPP` (Task 1); `FimTransport`, `chat_fim_hash` (Task 3).
- Produces: a foreign bench run whose stamp carries `runtime = spec.stored()`, `engine_build_commit = spec.version`, sentinel flags, `ctx = 0`, `n_parallel = 0`, empty `launch_args`.

**Key decisions (binding, from spec §2/§4/§5):**
- `--runtime` with more than one `--models` entry, or with any resolution that is not exactly one already-served subject, refuses with `RuntimeNeedsRunningServer` — the foreign path never launches the subject. Concretely: with `--runtime`, skip `live_pid`/`read_run_state`/`server_use_rule` entirely (a foreign server is not chekov's server and has no pid or run-state file); require `names.len() == 1` (else `RuntimeNeedsRunningServer`); the single candidate's action is `StepAction::UseRunning`.
- `--judge` stays permitted: the judge candidate launches chekov's own llama.cpp as today (spec §2).
- In `run_candidate`, the foreign candidate takes `pid = 0` and never calls `live_pid`, `launch`, or `teardown`; in `measure_candidate` the foreign branch calls `runtime::foreign_ready` instead of `candidate::ensure_ready`, prints `chekov: runtime <stored> serves: <ids joined ", ">` (or `serves: (none listed)` for an empty list), and proceeds with `PropsInfo { n_ctx: 0, total_slots: 0 }`.
- The upstream base URL is `--upstream` when given, else `cfg.base_url()` as today.
- `run_suites` selects `fim: FimTransport::Chat` exactly when the run's runtime is foreign.
- `build_head` foreign branch: skip `stamp_identity`'s engine lookup (machine_id is still required and still fails with `SetupIncomplete` when unknown); `engine = spec.version`; `flags` = all-`"unmanaged"` `StampedFlags`; `launch_args = Vec::new()`; `runtime = spec.stored()` on the stamp (llama.cpp branch stamps `RUNTIME_LLAMA_CPP`).
- `head_corpus`: when the codebase suite rides the chat arm, `prompt_set_hash = runner::chat_fim_hash(&base)` where `base` is today's value.

To keep every function ≤40 LOC and ≤3 params, thread ONE new bundle through the pipeline instead of loose params: add to `BenchArgs` a `runtime: Option<crate::core::bench::runtime::RuntimeSpec>` and `upstream: Option<String>` (parse `--runtime` in the `bench` entry before `resolve_candidates`, so a malformed value refuses before any other work), and carry `Option<&RuntimeSpec>` down via the existing `RunInputs`/`HeadInputs` bundles (add one field each). `From<&BenchOpts> for BenchArgs` cannot parse fallibly — move `BenchArgs` construction for the runtime field into `bench()` (or replace the `From` with a `fn bench_args(opts) -> Result<BenchArgs, ChekovError>`).

- [ ] **Step 1: Write the failing tests**

CLI parse (beside `bench_and_compare_parse` :2215, same `Wrap` pattern):

```rust
#[test]
fn runtime_parses_and_upstream_requires_it() {
    let w = Wrap::try_parse_from([
        "cap", "capability", "bench", "--runtime", "mtplx@0.4.1",
        "--upstream", "http://127.0.0.1:9999",
    ])
    .unwrap();
    // …destructure to BenchOpts as the sibling tests do, then:
    // assert_eq!(opts.runtime.as_deref(), Some("mtplx@0.4.1"));
    // assert_eq!(opts.upstream.as_deref(), Some("http://127.0.0.1:9999"));
    let _ = w;
    assert!(Wrap::try_parse_from([
        "cap", "capability", "bench", "--upstream", "http://x"
    ])
    .is_err());
}

#[test]
fn a_foreign_run_with_two_models_is_refused_before_any_http() {
    // Call the resolution helper directly with a parsed RuntimeSpec and
    // ["a","b"] and assert RuntimeNeedsRunningServer — shape this against
    // the helper you extract (see step 3); the point pinned here: the
    // refusal is pure, no Ctx, no server, no HTTP.
}

#[test]
fn a_foreign_stamp_is_sentinelled_and_named() {
    // Build the foreign StampedFlags + stamp through the same path
    // build_head's foreign branch uses (extract a pure helper:
    // foreign_stamp_parts(spec: &RuntimeSpec) -> (String, StampedFlags)
    // returning (engine, flags)); assert:
    //   engine == "0.4.1"
    //   every flag field == "unmanaged"
    // and via assemble_stamp with PropsInfo{0,0}: ctx == 0, n_parallel == 0,
    // runtime == "mtplx 0.4.1".
}
```

The second and third tests intentionally specify behaviour, not final signatures — write them against the helpers you extract in step 3, keeping the asserted facts exactly as commented. Also add: `the_chat_arm_is_selected_exactly_for_a_foreign_runtime` asserting the `FimTransport` chosen by the selection helper for `None` (llama.cpp) vs `Some(spec)`, and `a_foreign_codebase_hash_wraps_the_base` asserting `head_corpus`'s foreign result equals `runner::chat_fim_hash(&base)` for the same inputs.

- [ ] **Step 2: Watch them fail** — compile failure. Commit red: `test(capability): red — runtime flags, foreign refusals, sentinel stamp`.

- [ ] **Step 3: Implement** per the key decisions. `BenchOpts` additions:

```rust
    /// A foreign runtime serving the subject (`<name>@<version>`, e.g.
    /// `mtplx@0.4.1`). Declared, never probed: chekov measures a server YOU
    /// started, and refuses to launch one (spec 2026-08-31 §2).
    #[arg(long)]
    pub runtime: Option<String>,
    /// Base URL of the foreign server (default: the configured endpoint).
    #[arg(long, requires = "runtime")]
    pub upstream: Option<String>,
```

Sentinel constant lives beside `StampedFlags`:

```rust
/// A flag on a server chekov did not launch: not observed, not invented —
/// a third spelling distinct from "engine-default" (spec §5).
const FLAG_UNMANAGED: &str = "unmanaged";
```

with `fn unmanaged_flags() -> StampedFlags` filling all six fields. Extract small pure helpers so `bench`/`resolve_candidates`/`build_head` stay ≤40 LOC: `foreign_actions(spec, names) -> Result<Vec<StepAction>, ChekovError>` (the one-model check + `UseRunning`), `foreign_stamp_parts(spec) -> (String, StampedFlags)`, `fim_for(runtime: Option<&RuntimeSpec>) -> FimTransport`, and the serves-line printer. Dry-run: the plan step line for a foreign candidate must not claim a launch — `UseRunning` already renders as reuse (lifecycle.rs `step_line`); verify and leave as-is.

- [ ] **Step 4: Run** — `make lint && make test`, green; then live-verify the unchanged path only: `cargo run -- capability bench --dry-run` still prints today's plan (no server needed).
- [ ] **Step 5: Commit** — `feat(capability): bench --runtime/--upstream — the foreign UseRunning path with sentinel stamps`

---

### Task 5: `compare --cross-runtime`

**Files:**
- Modify: `src/core/bench/compare.rs:140-199` (`compare_runs`, `assert_same_environment`) + banner fn + tests
- Modify: `src/commands/capability.rs` — `CapAction`'s compare variant (:25 enum; find the variant's args) gains `--cross-runtime`; `compare` :2133-2153 threads it and prints the banner
- Test: `src/core/bench/compare.rs` tests module; a CLI parse test beside the others

**Interfaces:**
- Consumes: `Stamp.runtime`, `first_mismatch` (Task 1).
- Produces: `pub struct CompareOpts { pub significance_pct: f64, pub cross_runtime: bool }`; `compare_runs(a, b, opts: &CompareOpts)`; `pub fn cross_runtime_banner(a: &Stamp, b: &Stamp) -> String`.

- [ ] **Step 1: Write the failing tests** (compare.rs tests module; reuse its existing RunLog/stamp fixtures — find them via the tests around `assert_same_environment`'s callers)

```rust
#[test]
fn cross_runtime_masks_exactly_the_allow_list() {
    // Two fixture logs; set on b: runtime, engine_build_commit, ctx,
    // n_parallel, kv_unified, n_batch, n_ubatch, type_k, type_v,
    // flash_attn, prompt_set_hash all different from a.
    // compare_runs(&a, &b, &CompareOpts { significance_pct: 5.0,
    //   cross_runtime: true }) is Ok.
    // Then additionally set b.head.stamp.corpus_id = "other" and assert
    // Err(BenchStampMismatch { field: "corpus_id", .. }).
    // And WITHOUT the flag, the first pair refuses on "runtime".
}

#[test]
fn the_banner_names_each_differing_allow_listed_field_and_only_those() {
    // a llama.cpp stamp vs a foreign stamp differing on runtime,
    // engine_build_commit, flash_attn, prompt_set_hash:
    let banner = super::cross_runtime_banner(&a, &b);
    assert!(banner.starts_with("cross-runtime comparison: llama.cpp vs mtplx 0.4.1\n"));
    assert!(banner.contains("determinism does not hold across runtimes"));
    for needle in ["runtime: ", "engine_build_commit: ", "flash_attn: ", "prompt_set_hash: "] {
        assert!(banner.contains(needle), "missing {needle}");
    }
    assert!(!banner.contains("corpus_id"));
    assert!(banner.trim_end().ends_with("this measures the runtimes, not the model."));
}

#[test]
fn same_runtime_cross_runtime_banners_with_no_field_lines() {
    // identical stamps: banner still has the three fixed lines, zero
    // field lines between them.
}
```

- [ ] **Step 2: Watch them fail** — compile failure (`CompareOpts` unknown). Commit red: `test(compare): red — cross-runtime allow-list and banner`.

- [ ] **Step 3: Implement.** In compare.rs:

```rust
/// How a comparison runs: the significance threshold, and whether the
/// runtime allow-list (spec 2026-08-31 §7) is masked.
pub struct CompareOpts {
    pub significance_pct: f64,
    pub cross_runtime: bool,
}

/// The fields `--cross-runtime` permits to differ, and no others.
const CROSS_RUNTIME_ALLOWED: [&str; 11] = [
    "runtime", "engine_build_commit", "ctx", "n_parallel", "kv_unified",
    "n_batch", "n_ubatch", "type_k", "type_v", "flash_attn",
    "prompt_set_hash",
];
```

`compare_runs(a, b, opts: &CompareOpts)` (3 params kept); `assert_same_environment(a, b, cross_runtime: bool)` would be a boolean param — instead pass `opts` through: `assert_same_environment(pair: &RunPair, opts: &CompareOpts)`. Masking: extend the existing clone-and-copy pattern — when `opts.cross_runtime`, additionally `clone_from` a's `runtime`, `engine_build_commit`, `ctx`, `n_parallel`, `kv_unified`, `n_batch`, `n_ubatch`, `type_k`, `type_v`, `flash_attn`, `prompt_set_hash` onto `b_env` (copy for the two `u32`s). If that block pushes the function past 40 LOC, extract `fn mask_cross_runtime(b_env: &mut Stamp, a: &Stamp)`.

Banner (spec §7, verbatim shape):

```rust
/// The `--cross-runtime` banner: both runtimes, the warning, one line per
/// allow-listed field that differs, the closing sentence.
#[must_use]
pub fn cross_runtime_banner(a: &Stamp, b: &Stamp) -> String {
    let mut lines = vec![
        format!("cross-runtime comparison: {} vs {}", a.runtime, b.runtime),
        "determinism does not hold across runtimes; differing fields:".to_owned(),
    ];
    let (ja, jb) = (json_of(a), json_of(b));
    for field in CROSS_RUNTIME_ALLOWED {
        let (va, vb) = (&ja[field], &jb[field]);
        if va != vb {
            lines.push(format!("{field}: {va} vs {vb}"));
        }
    }
    lines.push("this measures the runtimes, not the model.".to_owned());
    lines.join("\n") + "\n"
}

fn json_of(s: &Stamp) -> serde_json::Value {
    serde_json::to_value(s).unwrap_or_default()
}
```

Command side: the compare CLI variant gains `#[arg(long)] cross_runtime: bool`; `compare()` builds `CompareOpts { significance_pct: f64::from(cfg…significance_pct), cross_runtime }`, and when the flag is set prints `cross_runtime_banner(&run_a.head.stamp, &run_b.head.stamp)` BEFORE `render_comparison` output (spec §7: banner before any section). Update `compare_runs`'s existing callers. Add a CLI parse test asserting `--cross-runtime` parses as a bare switch and defaults off.

- [ ] **Step 4: Run** — `make lint && make test`, green.
- [ ] **Step 5: Commit** — `feat(compare): --cross-runtime — the exact allow-list, masked subjects unchanged, loud banner`

---

### Task 6: Docs

**Files:**
- Modify: `README.md` (bench flags table/section: `--runtime`, `--upstream`; compare: `--cross-runtime`; one short "foreign runtimes" paragraph), `CHANGELOG.md` (unreleased entry), `IDEAS.md` (the 2026-08-30 foreign-runtime entry gains a SHIPPED paragraph in the house style — say what shipped, what was cut, and that live foreign verification is approval-gated and still owed)

**Interfaces:** none — text only, no `src/` changes.

- [ ] **Step 1: Write the docs.** Follow the existing README voice (one line per flag, honest caveats). The CHANGELOG entry names: the `runtime` stamp field (serde-default `llama.cpp`, stored runs unaffected), `bench --runtime/--upstream`, the chat-FIM transport and its `fim transport:` report line, `compare --cross-runtime` with the banner, the engine-neutral stamp-mismatch message. Check README for a stamp-field count or mismatch-message quote and update it if present.
- [ ] **Step 2: Verify** — `make lint && make test` (README fence tests, if any cover these sections, must parse); render-check any tables by eye.
- [ ] **Step 3: Commit** — `docs: foreign-runtime bench — README flags, changelog, IDEAS shipped`

---

## Self-Review (performed while writing)

- **Spec coverage:** §2 CLI → T4+T5; §3 stamp+message → T1; §4 readiness → T2+T4; §5 sentinels → T4; §6 chat FIM+hash+report line → T3+T4; §7 allow-list+banner → T5; §8 errors → T2; §9 tests → distributed per task; §10 out-of-scope respected (no launches, no registry change, no tune).
- **Placeholder scan:** the two `/* … */` blocks in T3 and the shaped-not-final tests in T4 are deliberate adapt-to-the-code instructions with the binding facts stated beside them, not TBDs; everything else is verbatim.
- **Type consistency:** `RuntimeSpec.stored()` produces the `runtime` string T1's stamp stores and T5's banner prints; `FimTransport::Chat` selection consumes `Option<&RuntimeSpec>`; `CompareOpts` replaces the bare `significance_pct` in both callers.
