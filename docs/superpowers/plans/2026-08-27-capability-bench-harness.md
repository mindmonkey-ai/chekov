# Capability Bench Harness (slice 5 remainder) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Finish slice 5 of the capability spec — `chekov capability bench [--fixture <path>]` and `chekov capability compare <a> <b>`: measure the running llama-server through chekov's own Anthropic↔OpenAI translator, store auditable run records, and compare runs with the statistical honesty rules already shipped in `core::stats`.

**Architecture:** A new `src/core/bench/` module tree (`runner`, `probes`, `fixture`, `grade`, `sweep`, `store`, `compare`) behind the existing `HttpClient` seam so every test injects canned responses — no network, no llama.cpp. The CLI grows two `CapAction` variants dispatched from `src/commands/capability.rs`. Timings come from llama-server's own `timings` object in the upstream OpenAI body (read BEFORE translation, which rightly drops them); the graded artifact is the ANTHROPIC body (the property pinned by `tests/bench_probe_crosses_the_translator.rs`).

**Tech Stack:** Rust (existing crate only — serde/serde_json/toml/ureq/nix/thiserror already in tree; **no new dependencies**).

**Spec:** Slice 5 of the capability spec (design session 2026-08-25). Verbatim:

> ### Slice 5 — `chekov capability bench --fixture` (~1,400 LOC)
> `runner.rs` (ClaudeFacade round trip, `/health`+pid readiness, `/props` assertion, flag hygiene, `timings`), `store.rs`, `stats.rs`, `sweep.rs`, `probes.rs`, `grade.rs`, `fixture.rs`, `compare`. The `--metric tok-s` grid upgrades from predicted to measured.
> *Acceptance test:* a test injects a canned OpenAI response through `ClaudeFacade::route` → `post_json` → `translate_response` and asserts the graded artifact is the **Anthropic** body; a test asserts `compare` refuses on a differing `engine.build_commit` naming that field; a test asserts a two-sample throughput point reports "insufficient depths to fit a curve" rather than extrapolating; a test asserts overlapping p10–p90 intervals with <5% median delta print `no significant difference`. **Release gate:** fixture-v1 does not ship until measured against three models of clearly different capability with the spread published in the slice notes.

Already shipped (PR #27, commit 884b046): `core::stats` (summarize / compare / can_fit_curve — acceptance tests 3 and 4) and the translator-crossing integration test (acceptance test 1). This plan builds the rest. Per the release gate, **no compiled-in fixture ships**: `--fixture <path>` loads a user-supplied TOML; fixture-v1 waits for the three-model measurement campaign.

Ground truth verified against the vendored engine (llama.cpp/tools/server):
- `/props` → `{"default_generation_settings": {"n_ctx": <slot ctx>, ...}, ...}` (server-context.cpp:4576).
- The non-streaming OpenAI-compat chat response carries `"timings": {"prompt_n", "prompt_per_second", "predicted_n", "predicted_per_second", ...}` whenever the slot ran (server-task.cpp:456-458, server-common.cpp:67-88).
- `/health` is public; `/props` sits behind `--api-key`.

## Global Constraints

- `make lint` (fmt + clippy `-D warnings`, pedantic/nursery) and `make test` green at every commit. clippy.toml encodes the size gates — do not touch it.
- Functions ≤40 LOC, ≤3 args (bundle into a struct beyond that), nesting ≤3, no boolean flag params.
- No `unwrap()`/`expect()` outside `#[cfg(test)]`. Errors are `thiserror` variants on `ChekovError` with remediation text.
- Every externally-deserialized struct: `#[serde(deny_unknown_fields)]`.
- No network in any test — everything crosses the `HttpClient` seam with canned fakes.
- No new dependencies, no new top-level directories in the repo. Run records live under `<CHEKOV_HOME>/logs/bench/`.
- Branch: `feat/capability-bench-harness` from `develop`. One commit per task.
- Pushkin gates writes; if a write is denied it names the rule — apply the fix and retry once.

---

### Task 1: `[bench]` config section

**Files:**
- Modify: `src/core/config.rs` (FileConfig + new BenchSection + `bench_dir()` + tests)
- Modify: `config.example.toml` (commented `[bench]` block, matching the file's existing style)

**Interfaces:**
- Produces: `cfg.file.bench: BenchSection { depths: Vec<u32>, repetitions: u32, max_tokens: u32, significance_pct: u32, ready_max_polls: u32, ready_interval_ms: u64 }`, `cfg.bench_dir() -> PathBuf` (= `logs/bench`). `significance_pct` is an integer percent so `FileConfig` keeps `Eq`.

- [ ] **Step 1: Write the failing tests** (in `src/core/config.rs` tests mod)

```rust
#[test]
fn bench_section_defaults_and_overrides_parse() {
    let cfg: super::FileConfig = toml::from_str("").expect("empty config is all defaults");
    assert_eq!(cfg.bench.depths, vec![1024, 4096, 16384]);
    assert_eq!(cfg.bench.repetitions, 5);
    assert_eq!(cfg.bench.significance_pct, 5);
    let cfg: super::FileConfig =
        toml::from_str("[bench]\ndepths = [2048]\nrepetitions = 3\n").expect("overrides parse");
    assert_eq!(cfg.bench.depths, vec![2048]);
    assert_eq!(cfg.bench.repetitions, 3);
    assert_eq!(cfg.bench.max_tokens, 128, "unset keys keep their defaults");
}

#[test]
fn bench_section_refuses_unknown_keys() {
    assert!(
        toml::from_str::<super::FileConfig>("[bench]\ntypo = 1\n").is_err(),
        "deny_unknown_fields (§C.7)"
    );
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test bench_section` → FAIL (no field `bench`).

- [ ] **Step 3: Implement**

```rust
/// `chekov capability bench` tunables (§6: knobs live here, not in code).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct BenchSection {
    /// Prompt depths (approximate tokens) the sweep measures, ascending.
    pub depths: Vec<u32>,
    /// Probes per depth; `core::stats` drops the first as warmup.
    pub repetitions: u32,
    /// Decode length per probe — long enough to measure, short enough to end.
    pub max_tokens: u32,
    /// Median delta (percent) below which two runs are "no significant difference".
    pub significance_pct: u32,
    /// Readiness poll budget: attempts × interval. 600 × 500ms covers the
    /// ~2-minute load of a ~158 GiB model with headroom.
    pub ready_max_polls: u32,
    pub ready_interval_ms: u64,
}

impl Default for BenchSection {
    fn default() -> Self {
        Self {
            depths: vec![1024, 4096, 16384],
            repetitions: 5,
            max_tokens: 128,
            significance_pct: 5,
            ready_max_polls: 600,
            ready_interval_ms: 500,
        }
    }
}
```

Add `pub bench: BenchSection` to `FileConfig`, and on `Config`:

```rust
/// Bench run records, one JSON file per run.
#[must_use]
pub fn bench_dir(&self) -> PathBuf {
    self.logs_dir().join("bench")
}
```

Add to `config.example.toml`, in the file's existing commented style:

```toml
# [bench]                        # `chekov capability bench` sweep
# depths = [1024, 4096, 16384]   # prompt depths (approx tokens)
# repetitions = 5                # per depth; the first is dropped as warmup
# max_tokens = 128               # decode length per probe
# significance_pct = 5           # median delta below this: no significant difference
# ready_max_polls = 600          # readiness budget: polls x interval
# ready_interval_ms = 500
```

- [ ] **Step 4: Run** `cargo test bench_section` → PASS; `make lint` clean.
- [ ] **Step 5: Commit** `feat(bench): [bench] config section with sweep and readiness tunables`

---

### Task 2: runner readiness (`/health`+pid) and `/props` assertion

**Files:**
- Create: `src/core/bench/mod.rs`, `src/core/bench/runner.rs`
- Modify: `src/core/mod.rs` (add `pub mod bench;`)
- Modify: `src/core/hub.rs` (default `get_auth` method + `UreqClient` override)
- Modify: `src/error.rs` (two variants)

**Interfaces:**
- Consumes: `HttpClient` (hub.rs), `server::process_alive(pid: i32) -> bool`, `BenchSection` (Task 1).
- Produces:
  - `bench::runner::UpstreamTarget { pub base_url: String, pub api_key: String }`
  - `bench::runner::ReadyTarget { pub base_url: String, pub pid: i32 }`
  - `bench::runner::ReadyPolicy { pub max_polls: u32, pub interval: Duration }` + `impl From<&BenchSection> for ReadyPolicy`
  - `pub fn wait_ready(http: &dyn HttpClient, target: &ReadyTarget, policy: ReadyPolicy) -> Result<(), ChekovError>`
  - `pub fn assert_props_ctx(http: &dyn HttpClient, upstream: &UpstreamTarget, expected: u32) -> Result<u32, ChekovError>`
  - `HttpClient::get_auth(&self, url, bearer: Option<&str>)` — default method delegating to `get` (fakes stay two-method); `UreqClient` sends the Authorization header.

- [ ] **Step 1: Write the failing tests** (`src/core/bench/runner.rs` tests mod)

```rust
use std::cell::RefCell;
use std::time::Duration;

use super::{ReadyPolicy, ReadyTarget, UpstreamTarget, assert_props_ctx, wait_ready};
use crate::core::hub::{HttpClient, JsonRequest};
use crate::error::ChekovError;

/// /health that answers 503-as-error `failures_left` times, then 200.
struct FlakyHealth {
    failures_left: RefCell<u32>,
}

impl HttpClient for FlakyHealth {
    fn get(&self, _url: &str) -> Result<String, ChekovError> {
        let mut left = self.failures_left.borrow_mut();
        if *left == 0 {
            return Ok(r#"{"status":"ok"}"#.into());
        }
        *left -= 1;
        Err(ChekovError::EndpointDown {
            url: "fake".into(),
            reason: "503 while loading".into(),
        })
    }

    fn post_json(&self, _req: &JsonRequest) -> Result<String, ChekovError> {
        unreachable!("readiness never POSTs")
    }
}

fn own_pid() -> i32 {
    i32::try_from(std::process::id()).expect("pid fits")
}

fn instant_policy(max_polls: u32) -> ReadyPolicy {
    ReadyPolicy { max_polls, interval: Duration::ZERO }
}

#[test]
fn readiness_waits_through_loading_then_succeeds() {
    let http = FlakyHealth { failures_left: RefCell::new(2) };
    let target = ReadyTarget { base_url: "http://fake".into(), pid: own_pid() };
    wait_ready(&http, &target, instant_policy(5)).expect("ready on the third poll");
}

#[test]
fn a_dead_pid_fails_as_died_not_as_a_timeout() {
    // The server exiting during load must be reported as its own failure —
    // a timeout message would send the user waiting instead of to the log.
    let http = FlakyHealth { failures_left: RefCell::new(u32::MAX) };
    let target = ReadyTarget { base_url: "http://fake".into(), pid: 99_999_999 };
    let err = wait_ready(&http, &target, instant_policy(5)).expect_err("died");
    assert!(matches!(err, ChekovError::ServerDiedWhileLoading { pid: 99_999_999 }));
}

#[test]
fn readiness_gives_up_after_the_poll_budget() {
    let http = FlakyHealth { failures_left: RefCell::new(u32::MAX) };
    let target = ReadyTarget { base_url: "http://fake".into(), pid: own_pid() };
    let err = wait_ready(&http, &target, instant_policy(3)).expect_err("budget spent");
    assert!(matches!(err, ChekovError::EndpointDown { .. }));
}

/// Canned /props body; records the bearer it was asked with.
struct CannedProps {
    body: String,
    bearer_seen: RefCell<Option<String>>,
}

impl HttpClient for CannedProps {
    fn get(&self, _url: &str) -> Result<String, ChekovError> {
        Ok(self.body.clone())
    }

    fn get_auth(&self, url: &str, bearer: Option<&str>) -> Result<String, ChekovError> {
        *self.bearer_seen.borrow_mut() = bearer.map(str::to_owned);
        self.get(url)
    }

    fn post_json(&self, _req: &JsonRequest) -> Result<String, ChekovError> {
        unreachable!("/props is a GET")
    }
}

fn props(n_ctx: u64) -> CannedProps {
    CannedProps {
        body: serde_json::json!({"default_generation_settings": {"n_ctx": n_ctx}}).to_string(),
        bearer_seen: RefCell::new(None),
    }
}

fn upstream() -> UpstreamTarget {
    UpstreamTarget { base_url: "http://fake".into(), api_key: "sekrit".into() }
}

#[test]
fn a_matching_props_ctx_passes_and_carries_the_api_key() {
    let http = props(131_072);
    let got = assert_props_ctx(&http, &upstream(), 131_072).expect("matches");
    assert_eq!(got, 131_072);
    assert_eq!(
        http.bearer_seen.borrow().as_deref(),
        Some("sekrit"),
        "/props sits behind --api-key; the probe must authenticate"
    );
}

#[test]
fn a_mismatched_props_ctx_is_refused_naming_both_numbers() {
    // The server loaded something other than what the registry intended —
    // benching it would attribute the numbers to a config that is not running.
    let http = props(65_536);
    let err = assert_props_ctx(&http, &upstream(), 131_072).expect_err("mismatch");
    assert!(matches!(
        err,
        ChekovError::PropsCtxMismatch { server: 65_536, config: 131_072 }
    ));
}

#[test]
fn props_without_n_ctx_is_loud_rather_than_assumed() {
    let http = CannedProps {
        body: r#"{"total_slots": 4}"#.into(),
        bearer_seen: RefCell::new(None),
    };
    assert!(assert_props_ctx(&http, &upstream(), 131_072).is_err());
}
```

- [ ] **Step 2: Run** `cargo test bench::runner` → FAIL (module missing).

- [ ] **Step 3: Implement**

`src/core/mod.rs`: add `pub mod bench;` (alphabetical position).

`src/core/bench/mod.rs`:

```rust
//! `chekov capability bench` — measured throughput and graded probes through
//! chekov's OWN Anthropic<->OpenAI translator, so every number was earned on
//! the exact code path a Claude Code turn takes.

pub mod runner;
```

`src/error.rs` — add (near the other server variants):

```rust
#[error(
    "llama-server (pid {pid}) exited while chekov waited for it to become \
     ready — read the tail of logs/llama-server.log"
)]
ServerDiedWhileLoading { pid: i32 },

#[error(
    "the server loaded n_ctx {server} but the effective config says {config} — \
     a bench against the wrong context would be recorded under a config the \
     server is not running; `chekov restart` and re-run"
)]
PropsCtxMismatch { server: u32, config: u32 },
```

`src/core/hub.rs` — add to the `HttpClient` trait and `UreqClient`:

```rust
/// GET with an optional bearer token. Defaults to the plain `get` so the
/// canned test fakes stay two-method; the production client sends the header.
fn get_auth(&self, url: &str, bearer: Option<&str>) -> Result<String, ChekovError> {
    let _ = bearer;
    self.get(url)
}
```

```rust
fn get_auth(&self, url: &str, bearer: Option<&str>) -> Result<String, ChekovError> {
    let mut builder = ureq::get(url);
    if let Some(token) = bearer {
        builder = builder.header("Authorization", &format!("Bearer {token}"));
    }
    let mut response = builder.call().map_err(|e| ChekovError::EndpointDown {
        url: url.to_owned(),
        reason: e.to_string(),
    })?;
    response
        .body_mut()
        .read_to_string()
        .map_err(|e| ChekovError::EndpointDown {
            url: url.to_owned(),
            reason: e.to_string(),
        })
}
```

`src/core/bench/runner.rs`:

```rust
//! Server readiness and the `/props` assertion.
//!
//! Readiness is `/health` AND the pid: a server that dies while loading must
//! fail as "died" (go read the log), never as a timeout (keep waiting).

use std::time::Duration;

use serde_json::Value;

use crate::core::config::BenchSection;
use crate::core::hub::HttpClient;
use crate::error::ChekovError;

/// Where the llama-server answers, and how to authenticate to it.
pub struct UpstreamTarget {
    pub base_url: String,
    pub api_key: String,
}

/// What readiness watches: the health endpoint and the process behind it.
pub struct ReadyTarget {
    pub base_url: String,
    pub pid: i32,
}

/// Poll budget, from `[bench]` config.
#[derive(Debug, Clone, Copy)]
pub struct ReadyPolicy {
    pub max_polls: u32,
    pub interval: Duration,
}

impl From<&BenchSection> for ReadyPolicy {
    fn from(bench: &BenchSection) -> Self {
        Self {
            max_polls: bench.ready_max_polls,
            interval: Duration::from_millis(bench.ready_interval_ms),
        }
    }
}

/// Wait until `/health` answers, watching the pid between polls.
pub fn wait_ready(
    http: &dyn HttpClient,
    target: &ReadyTarget,
    policy: ReadyPolicy,
) -> Result<(), ChekovError> {
    let url = format!("{}/health", target.base_url);
    for _ in 0..policy.max_polls {
        if !crate::core::server::process_alive(target.pid) {
            return Err(ChekovError::ServerDiedWhileLoading { pid: target.pid });
        }
        // /health is public; 503-while-loading surfaces as Err and we keep polling.
        if http.get(&url).is_ok() {
            return Ok(());
        }
        std::thread::sleep(policy.interval);
    }
    Err(ChekovError::EndpointDown {
        url,
        reason: format!("not ready after {} polls", policy.max_polls),
    })
}

/// The context the server ACTUALLY loaded, asserted against the config's intent.
pub fn assert_props_ctx(
    http: &dyn HttpClient,
    upstream: &UpstreamTarget,
    expected: u32,
) -> Result<u32, ChekovError> {
    let url = format!("{}/props", upstream.base_url);
    let raw = http.get_auth(&url, Some(&upstream.api_key))?;
    let parsed: Value =
        serde_json::from_str(&raw).map_err(|e| ChekovError::EndpointDown {
            url: url.clone(),
            reason: format!("/props is not JSON: {e}"),
        })?;
    let n_ctx = parsed
        .pointer("/default_generation_settings/n_ctx")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .ok_or(ChekovError::EndpointDown {
            url,
            reason: "no default_generation_settings.n_ctx in /props".to_owned(),
        })?;
    if n_ctx == expected {
        Ok(n_ctx)
    } else {
        Err(ChekovError::PropsCtxMismatch { server: n_ctx, config: expected })
    }
}
```

- [ ] **Step 4: Run** `cargo test bench::runner` → PASS; `make lint`.
- [ ] **Step 5: Commit** `feat(bench): /health+pid readiness and the /props context assertion`

---

### Task 3: probe crossing — round trip with `timings` capture

**Files:**
- Modify: `src/core/bench/runner.rs` (add `ProbeArtifact`, `Timings`, `ProbeWire`, `cross`, `read_timings` + tests)
- Modify: `src/error.rs` (`BenchNoTimings`)

**Interfaces:**
- Consumes: `AgentFacade` / `Action` / `Forward` (proxy), `JsonRequest` (hub), `UpstreamTarget` (Task 2).
- Produces:
  - `pub struct Timings { pub prompt_n: u64, pub prompt_per_second: f64, pub predicted_n: u64, pub predicted_per_second: f64 }`
  - `pub struct ProbeArtifact { pub anthropic_body: String, pub timings: Timings }`
  - `pub struct ProbeWire<'a> { pub http: &'a dyn HttpClient, pub facade: &'a dyn AgentFacade, pub upstream: &'a UpstreamTarget }`
  - `pub fn cross(wire: &ProbeWire, req: &HttpRequest) -> Result<ProbeArtifact, ChekovError>`

- [ ] **Step 1: Write the failing tests** (runner tests mod)

```rust
use crate::core::proxy::claude::ClaudeFacade;
use crate::core::proxy::http::HttpRequest;

/// Upstream answering every POST with one canned OpenAI body.
struct CannedUpstream {
    body: String,
    bearer_seen: RefCell<Option<String>>,
}

impl HttpClient for CannedUpstream {
    fn get(&self, _url: &str) -> Result<String, ChekovError> {
        unreachable!("a probe crossing never GETs")
    }

    fn post_json(&self, req: &JsonRequest) -> Result<String, ChekovError> {
        *self.bearer_seen.borrow_mut() = req.bearer.clone();
        Ok(self.body.clone())
    }
}

fn anthropic_request(prompt: &str) -> HttpRequest {
    let body = serde_json::json!({
        "model": "claude-sonnet-4",
        "max_tokens": 64,
        "messages": [{ "role": "user", "content": prompt }],
    });
    HttpRequest {
        method: "POST".into(),
        path: "/v1/messages".into(),
        body: body.to_string().into_bytes(),
    }
}

fn openai_with_timings() -> String {
    serde_json::json!({
        "choices": [{ "message": { "content": "hello there" }, "finish_reason": "stop" }],
        "usage": { "prompt_tokens": 900, "completion_tokens": 100 },
        "timings": {
            "prompt_n": 900, "prompt_ms": 2000.0, "prompt_per_second": 450.0,
            "predicted_n": 100, "predicted_ms": 4608.3, "predicted_per_second": 21.7
        }
    })
    .to_string()
}

#[test]
fn cross_returns_the_anthropic_body_and_the_upstream_timings() {
    let http = CannedUpstream { body: openai_with_timings(), bearer_seen: RefCell::new(None) };
    let facade = ClaudeFacade::new("local-model");
    let up = upstream();
    let wire = super::ProbeWire { http: &http, facade: &facade, upstream: &up };
    let art = super::cross(&wire, &anthropic_request("say hi")).expect("crossing succeeds");
    // Timings are the server's own measurement, read before translation.
    assert!((art.timings.predicted_per_second - 21.7).abs() < 1e-9);
    assert_eq!(art.timings.prompt_n, 900);
    // The artifact is the ANTHROPIC body — the bytes an agent would parse.
    let graded: serde_json::Value =
        serde_json::from_str(&art.anthropic_body).expect("artifact is json");
    assert_eq!(graded["content"][0]["text"], "hello there");
    assert!(graded.get("choices").is_none(), "translator was bypassed: {graded}");
    assert_eq!(http.bearer_seen.borrow().as_deref(), Some("sekrit"));
}

#[test]
fn a_response_without_timings_fails_rather_than_inventing_a_number() {
    let no_timings = serde_json::json!({
        "choices": [{ "message": { "content": "hi" }, "finish_reason": "stop" }]
    })
    .to_string();
    let http = CannedUpstream { body: no_timings, bearer_seen: RefCell::new(None) };
    let facade = ClaudeFacade::new("m");
    let up = upstream();
    let wire = super::ProbeWire { http: &http, facade: &facade, upstream: &up };
    let err = super::cross(&wire, &anthropic_request("hi")).expect_err("no measurement");
    assert!(matches!(err, ChekovError::BenchNoTimings));
}

#[test]
fn a_locally_answered_request_cannot_be_a_probe() {
    // GET /v1/models is answered by the facade without touching the server —
    // "measuring" it would time chekov, not the model.
    let http = CannedUpstream { body: String::new(), bearer_seen: RefCell::new(None) };
    let facade = ClaudeFacade::new("m");
    let up = upstream();
    let wire = super::ProbeWire { http: &http, facade: &facade, upstream: &up };
    let req = HttpRequest { method: "GET".into(), path: "/v1/models".into(), body: vec![] };
    assert!(super::cross(&wire, &req).is_err());
}
```

- [ ] **Step 2: Run** → FAIL (no `cross`).

- [ ] **Step 3: Implement** (append to runner.rs)

```rust
/// llama-server's own measurement of one exchange, from its `timings` object.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Timings {
    pub prompt_n: u64,
    pub prompt_per_second: f64,
    pub predicted_n: u64,
    pub predicted_per_second: f64,
}

/// One measured probe: what the agent would receive, and what it cost.
pub struct ProbeArtifact {
    /// The Anthropic-shaped body — the same bytes an agent would parse.
    pub anthropic_body: String,
    pub timings: Timings,
}

/// Everything one crossing needs, bundled (§4).
pub struct ProbeWire<'a> {
    pub http: &'a dyn HttpClient,
    pub facade: &'a dyn AgentFacade,
    pub upstream: &'a UpstreamTarget,
}

/// Route → POST upstream → capture `timings` → translate.
///
/// Timings are read from the upstream OpenAI body BEFORE translation (the
/// translator rightly drops them); the artifact handed to grading is the
/// Anthropic body, per `tests/bench_probe_crosses_the_translator.rs`.
pub fn cross(wire: &ProbeWire, req: &HttpRequest) -> Result<ProbeArtifact, ChekovError> {
    let forward = match wire.facade.route(req)? {
        Action::Forward(f) => f,
        Action::Reply(_) => {
            return Err(ChekovError::ProxyBadRequest {
                reason: "a bench probe must forward upstream; this request was \
                         answered locally"
                    .to_owned(),
            });
        }
    };
    let body = String::from_utf8(forward.body).map_err(|e| ChekovError::ProxyBadRequest {
        reason: format!("forwarded body is not UTF-8: {e}"),
    })?;
    let upstream_body = wire.http.post_json(&JsonRequest {
        url: format!("{}{}", wire.upstream.base_url, forward.path),
        body,
        bearer: Some(wire.upstream.api_key.clone()),
    })?;
    let timings = read_timings(&upstream_body)?;
    let anthropic_body = wire.facade.translate_response(&upstream_body)?;
    Ok(ProbeArtifact { anthropic_body, timings })
}

/// The four numbers the sweep records. All-or-nothing: a partial timings
/// object must not become a partial measurement.
fn read_timings(upstream: &str) -> Result<Timings, ChekovError> {
    let parsed: Value = serde_json::from_str(upstream).map_err(|_| ChekovError::BenchNoTimings)?;
    let t = parsed.get("timings").ok_or(ChekovError::BenchNoTimings)?;
    let float = |key: &str| t.get(key).and_then(Value::as_f64);
    let count = |key: &str| t.get(key).and_then(Value::as_u64);
    match (
        count("prompt_n"),
        float("prompt_per_second"),
        count("predicted_n"),
        float("predicted_per_second"),
    ) {
        (Some(prompt_n), Some(pps), Some(predicted_n), Some(gps)) => Ok(Timings {
            prompt_n,
            prompt_per_second: pps,
            predicted_n,
            predicted_per_second: gps,
        }),
        _ => Err(ChekovError::BenchNoTimings),
    }
}
```

Imports to add at the top of runner.rs: `use crate::core::hub::JsonRequest;`, `use crate::core::proxy::http::HttpRequest;`, `use crate::core::proxy::{Action, AgentFacade};`.

`src/error.rs`:

```rust
#[error(
    "the upstream response carries no timings object — chekov never invents a \
     measurement; rebuild the engine (`chekov update --engine`) and retry"
)]
BenchNoTimings,
```

- [ ] **Step 4: Run** `cargo test bench::runner` → PASS; `make lint`.
- [ ] **Step 5: Commit** `feat(bench): probe crossing captures upstream timings and the Anthropic artifact`

---

### Task 4: depth-targeted throughput probes

**Files:**
- Create: `src/core/bench/probes.rs` (+ register in bench/mod.rs)

**Interfaces:**
- Produces: `pub fn throughput_probe(depth_tokens: u32, max_tokens: u32) -> HttpRequest`; `pub(crate) fn anthropic_post(body: &serde_json::Value) -> HttpRequest` (reused by Task 5's `fixture_probe`).

- [ ] **Step 1: Write the failing tests**

```rust
use crate::core::proxy::claude::ClaudeFacade;
use crate::core::proxy::{Action, AgentFacade};

#[test]
fn a_deeper_probe_carries_a_proportionally_longer_prompt() {
    let shallow = super::throughput_probe(1_024, 64);
    let deep = super::throughput_probe(16_384, 64);
    assert!(
        deep.body.len() > shallow.body.len() * 8,
        "16x the depth must be roughly 16x the filler: {} vs {}",
        deep.body.len(),
        shallow.body.len()
    );
}

#[test]
fn a_probe_is_anthropic_shaped_and_crosses_the_translator() {
    let req = super::throughput_probe(64, 16);
    assert_eq!(req.path, "/v1/messages", "probes speak the agent's dialect");
    let facade = ClaudeFacade::new("local-model");
    match facade.route(&req).expect("routable") {
        Action::Forward(f) => {
            let sent: serde_json::Value =
                serde_json::from_slice(&f.body).expect("forwarded body is json");
            assert_eq!(sent["model"], "local-model");
            assert_eq!(sent["max_tokens"], 16);
            assert_eq!(f.path, "/v1/chat/completions");
        }
        Action::Reply(_) => panic!("a probe must go upstream"),
    }
}
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement** `src/core/bench/probes.rs`:

```rust
//! Depth-targeted probe requests, Anthropic-shaped as Claude Code would send
//! them — a probe that skipped the translator would measure a server chekov
//! does not actually serve.

use crate::core::proxy::http::HttpRequest;

/// Filler-word count per requested token. Common short words tokenize near
/// 1:1; the HONEST depth is `timings.prompt_n`, which the sweep records.
const WORDS_PER_TOKEN: usize = 1;

/// A probe whose prompt approximates `depth_tokens` and whose reply exercises
/// decode for up to `max_tokens`.
#[must_use]
pub fn throughput_probe(depth_tokens: u32, max_tokens: u32) -> HttpRequest {
    let filler = "lorem ".repeat(depth_tokens as usize * WORDS_PER_TOKEN);
    anthropic_post(&serde_json::json!({
        "model": "claude-sonnet-4",
        "max_tokens": max_tokens,
        "system": filler,
        "messages": [{
            "role": "user",
            "content": "Count upward from one, one number per line, and do not stop."
        }],
    }))
}

/// POST `/v1/messages` with `body`, exactly as an Anthropic SDK client would.
pub(crate) fn anthropic_post(body: &serde_json::Value) -> HttpRequest {
    HttpRequest {
        method: "POST".into(),
        path: "/v1/messages".into(),
        body: body.to_string().into_bytes(),
    }
}
```

Add `pub mod probes;` to bench/mod.rs.

- [ ] **Step 4: Run** `cargo test bench::probes` → PASS; `make lint`.
- [ ] **Step 5: Commit** `feat(bench): depth-targeted throughput probes in the agent's dialect`

---

### Task 5: fixture loading and grading

**Files:**
- Create: `src/core/bench/fixture.rs`, `src/core/bench/grade.rs` (+ register both)
- Modify: `src/core/bench/probes.rs` (add `fixture_probe`)
- Modify: `src/error.rs` (`FixtureInvalid`)

**Interfaces:**
- Produces:
  - `fixture::Fixture { pub version: u32, pub probes: Vec<FixtureProbe> }`
  - `fixture::FixtureProbe { pub id: String, pub prompt: String, pub max_tokens: u32, pub expect_contains: Vec<String> }`
  - `pub fn fixture::load(path: &Path) -> Result<Fixture, ChekovError>`
  - `grade::Grade { Pass, Fail { reason: String } }` (+ `is_pass()`, `reason()` accessors)
  - `pub fn grade::grade(anthropic_body: &str, probe: &FixtureProbe) -> Grade`
  - `probes::fixture_probe(probe: &FixtureProbe) -> HttpRequest`

- [ ] **Step 1: Write the failing tests**

`fixture.rs` tests:

```rust
use std::path::PathBuf;

fn write_scratch(name: &str, text: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("chekov-test-fixture");
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let path = dir.join(name);
    std::fs::write(&path, text).expect("write fixture");
    path
}

#[test]
fn a_valid_fixture_parses() {
    let path = write_scratch(
        "ok.toml",
        r#"
version = 1

[[probes]]
id = "greeting"
prompt = "Say hello."
max_tokens = 32
expect_contains = ["hello"]
"#,
    );
    let f = super::load(&path).expect("valid fixture");
    assert_eq!(f.probes.len(), 1);
    assert_eq!(f.probes[0].id, "greeting");
}

#[test]
fn an_unknown_key_is_refused() {
    let path = write_scratch("typo.toml", "version = 1\nprobes = []\ntypo = 1\n");
    assert!(super::load(&path).is_err(), "deny_unknown_fields");
}

#[test]
fn a_newer_version_is_refused_naming_what_this_chekov_reads() {
    let path = write_scratch("v2.toml", "version = 2\nprobes = []\n");
    let err = super::load(&path).expect_err("too new");
    assert!(err.to_string().contains("version 1"), "{err}");
}

#[test]
fn an_empty_probe_list_is_refused() {
    let path = write_scratch("empty.toml", "version = 1\nprobes = []\n");
    assert!(super::load(&path).is_err(), "a fixture with nothing to grade is a mistake");
}
```

`grade.rs` tests:

```rust
use super::{Grade, grade};
use crate::core::bench::fixture::FixtureProbe;

fn probe(expect: &[&str]) -> FixtureProbe {
    FixtureProbe {
        id: "p".into(),
        prompt: "q".into(),
        max_tokens: 32,
        expect_contains: expect.iter().map(|s| (*s).to_owned()).collect(),
    }
}

fn anthropic(text: &str) -> String {
    serde_json::json!({
        "type": "message",
        "content": [{"type": "text", "text": text}],
    })
    .to_string()
}

#[test]
fn a_reply_containing_every_expected_substring_passes_case_insensitively() {
    assert!(matches!(grade(&anthropic("Hello, World"), &probe(&["hello", "world"])), Grade::Pass));
}

#[test]
fn a_missing_substring_fails_naming_it() {
    match grade(&anthropic("hi"), &probe(&["hello"])) {
        Grade::Fail { reason } => assert!(reason.contains("hello"), "{reason}"),
        Grade::Pass => panic!("must fail"),
    }
}

#[test]
fn an_empty_reply_fails_rather_than_passing_an_expectation_free_probe() {
    assert!(matches!(grade(&anthropic("  "), &probe(&[])), Grade::Fail { .. }));
}

#[test]
fn a_body_without_content_text_fails_as_a_translation_problem_not_an_empty_reply() {
    // A grader that reads a broken artifact as "the model said nothing" would
    // score a broken server as a merely unhelpful model.
    let broken = r#"{"choices":[{"message":{"content":"hi"}}]}"#;
    match grade(broken, &probe(&[])) {
        Grade::Fail { reason } => assert!(reason.contains("content"), "{reason}"),
        Grade::Pass => panic!("must fail"),
    }
}
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement**

`src/core/bench/fixture.rs`:

```rust
//! User-supplied graded probe sets (TOML).
//!
//! Deliberately NO compiled-in fixture: fixture-v1 carries a release gate —
//! it does not ship until measured against three models of clearly different
//! capability with the spread published — so until that campaign happens,
//! `--fixture <path>` is the only source of graded probes.

use std::path::Path;

use serde::Deserialize;

use crate::error::ChekovError;

/// What this chekov knows how to read.
const SUPPORTED_VERSION: u32 = 1;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fixture {
    pub version: u32,
    pub probes: Vec<FixtureProbe>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureProbe {
    pub id: String,
    pub prompt: String,
    pub max_tokens: u32,
    /// Substrings the reply must contain (all of them), case-insensitive.
    #[serde(default)]
    pub expect_contains: Vec<String>,
}

pub fn load(path: &Path) -> Result<Fixture, ChekovError> {
    let invalid = |reason: String| ChekovError::FixtureInvalid {
        path: path.to_path_buf(),
        reason,
    };
    let text = std::fs::read_to_string(path).map_err(|e| invalid(e.to_string()))?;
    let fixture: Fixture = toml::from_str(&text).map_err(|e| invalid(e.to_string()))?;
    if fixture.version != SUPPORTED_VERSION {
        return Err(invalid(format!(
            "version {} — this chekov reads version {SUPPORTED_VERSION}",
            fixture.version
        )));
    }
    if fixture.probes.is_empty() {
        return Err(invalid("no probes — a fixture with nothing to grade".to_owned()));
    }
    Ok(fixture)
}
```

`src/core/bench/grade.rs`:

```rust
//! Grading reads the ANTHROPIC artifact — the same bytes the agent would
//! parse. A body the translator could not produce fails the probe; it never
//! grades as an empty (merely unhelpful) reply.

use serde_json::Value;

use super::fixture::FixtureProbe;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Grade {
    Pass,
    Fail { reason: String },
}

#[must_use]
pub fn grade(anthropic_body: &str, probe: &FixtureProbe) -> Grade {
    let Ok(parsed) = serde_json::from_str::<Value>(anthropic_body) else {
        return Grade::Fail { reason: "artifact is not JSON".to_owned() };
    };
    let Some(text) = parsed.pointer("/content/0/text").and_then(Value::as_str) else {
        return Grade::Fail {
            reason: "no content[0].text in the artifact — a translation failure, \
                     not an empty reply"
                .to_owned(),
        };
    };
    if text.trim().is_empty() {
        return Grade::Fail { reason: "empty reply".to_owned() };
    }
    let lower = text.to_lowercase();
    for expected in &probe.expect_contains {
        if !lower.contains(&expected.to_lowercase()) {
            return Grade::Fail { reason: format!("missing expected substring {expected:?}") };
        }
    }
    Grade::Pass
}
```

`probes.rs` addition:

```rust
use super::fixture::FixtureProbe;

/// A graded probe from a fixture, in the same dialect as every other probe.
#[must_use]
pub fn fixture_probe(probe: &FixtureProbe) -> HttpRequest {
    anthropic_post(&serde_json::json!({
        "model": "claude-sonnet-4",
        "max_tokens": probe.max_tokens,
        "messages": [{"role": "user", "content": probe.prompt}],
    }))
}
```

`src/error.rs`:

```rust
#[error("bench fixture {path}: {reason}")]
FixtureInvalid { path: PathBuf, reason: String },
```

(`PathBuf` display: use `{}` with `path.display()` — match the existing `ConfigInvalid` variant's formatting style, i.e. `#[error("bench fixture {}: {reason}", path.display())]`.)

Register `pub mod fixture; pub mod grade;` in bench/mod.rs.

- [ ] **Step 4: Run** `cargo test bench::` → PASS; `make lint`.
- [ ] **Step 5: Commit** `feat(bench): fixture loading and Anthropic-artifact grading (fixture-v1 stays release-gated)`

---

### Task 6: the sweep

**Files:**
- Create: `src/core/bench/sweep.rs` (+ register)

**Interfaces:**
- Consumes: `probes::throughput_probe`, `runner::ProbeArtifact`, `stats::{summarize, can_fit_curve, Summary}`, `BenchSection`.
- Produces:
  - `pub struct SweepPlan { pub depths: Vec<u32>, pub repetitions: u32, pub max_tokens: u32 }` + `impl From<&BenchSection> for SweepPlan`
  - `pub struct DepthResult { pub depth: u32, pub prompt_n: u64, pub decode_samples: Vec<f64>, pub prefill_samples: Vec<f64>, pub decode: Option<Summary>, pub prefill: Option<Summary> }`
  - `pub type ProbeExec<'a> = dyn FnMut(&HttpRequest) -> Result<ProbeArtifact, ChekovError> + 'a`
  - `pub fn run_sweep(plan: &SweepPlan, exec: &mut ProbeExec) -> Result<Vec<DepthResult>, ChekovError>`
  - `pub fn curve_note(distinct_depths: usize) -> Option<String>` — `Some("insufficient depths to fit a curve — measure at least 3 (got N)")` when `!stats::can_fit_curve(N)`.

- [ ] **Step 1: Write the failing tests**

```rust
use super::{SweepPlan, curve_note, run_sweep};
use crate::core::bench::runner::{ProbeArtifact, Timings};
use crate::error::ChekovError;

fn artifact(decode_tps: f64) -> ProbeArtifact {
    ProbeArtifact {
        anthropic_body: r#"{"type":"message","content":[{"type":"text","text":"1"}]}"#.into(),
        timings: Timings {
            prompt_n: 1000,
            prompt_per_second: 400.0,
            predicted_n: 128,
            predicted_per_second: decode_tps,
        },
    }
}

#[test]
fn a_sweep_summarises_each_depth_and_keeps_the_raw_samples() {
    let plan = SweepPlan { depths: vec![100, 200], repetitions: 3, max_tokens: 16 };
    let mut tick = 0_u32;
    let results = run_sweep(&plan, &mut |_req| {
        tick += 1;
        Ok(artifact(20.0 + f64::from(tick)))
    })
    .expect("sweep");
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].decode_samples.len(), 3, "raw samples are auditable");
    let summary = results[0].decode.as_ref().expect("three samples summarise");
    assert_eq!(summary.warmup_dropped, 1);
    assert_eq!(results[0].prompt_n, 1000, "the honest depth is the measured one");
}

#[test]
fn a_failed_probe_fails_the_sweep_loudly() {
    let plan = SweepPlan { depths: vec![100], repetitions: 2, max_tokens: 16 };
    let result = run_sweep(&plan, &mut |_req| {
        Err(ChekovError::BenchNoTimings)
    });
    assert!(result.is_err(), "a mid-sweep failure must not yield a partial run");
}

#[test]
fn fewer_than_three_depths_refuse_a_curve_in_the_stated_words() {
    let note = curve_note(2).expect("two depths cannot fit a curve");
    assert!(note.contains("insufficient depths to fit a curve"), "{note}");
    assert!(note.contains("(got 2)"), "{note}");
    assert_eq!(curve_note(3), None);
}
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement** `src/core/bench/sweep.rs`:

```rust
//! Depth × repetition sweep, summarised by `core::stats` — the raw samples
//! ride along so every summary can be audited back to what was measured.

use crate::core::bench::probes;
use crate::core::bench::runner::ProbeArtifact;
use crate::core::config::BenchSection;
use crate::core::proxy::http::HttpRequest;
use crate::core::stats::{self, Summary};
use crate::error::ChekovError;

pub struct SweepPlan {
    pub depths: Vec<u32>,
    pub repetitions: u32,
    pub max_tokens: u32,
}

impl From<&BenchSection> for SweepPlan {
    fn from(bench: &BenchSection) -> Self {
        Self {
            depths: bench.depths.clone(),
            repetitions: bench.repetitions,
            max_tokens: bench.max_tokens,
        }
    }
}

/// One depth's measurements: raw samples plus their summaries.
pub struct DepthResult {
    pub depth: u32,
    /// Measured prompt depth (`timings.prompt_n`) — the honest x-axis.
    pub prompt_n: u64,
    pub decode_samples: Vec<f64>,
    pub prefill_samples: Vec<f64>,
    pub decode: Option<Summary>,
    pub prefill: Option<Summary>,
}

/// The probe executor — `runner::cross` in production, canned in tests.
pub type ProbeExec<'a> = dyn FnMut(&HttpRequest) -> Result<ProbeArtifact, ChekovError> + 'a;

pub fn run_sweep(plan: &SweepPlan, exec: &mut ProbeExec) -> Result<Vec<DepthResult>, ChekovError> {
    plan.depths
        .iter()
        .map(|&depth| measure_depth(plan, depth, exec))
        .collect()
}

fn measure_depth(
    plan: &SweepPlan,
    depth: u32,
    exec: &mut ProbeExec,
) -> Result<DepthResult, ChekovError> {
    let mut decode_samples = Vec::new();
    let mut prefill_samples = Vec::new();
    let mut prompt_n = 0_u64;
    for _ in 0..plan.repetitions {
        let artifact = exec(&probes::throughput_probe(depth, plan.max_tokens))?;
        decode_samples.push(artifact.timings.predicted_per_second);
        prefill_samples.push(artifact.timings.prompt_per_second);
        prompt_n = prompt_n.max(artifact.timings.prompt_n);
    }
    Ok(DepthResult {
        depth,
        prompt_n,
        decode: stats::summarize(&decode_samples),
        prefill: stats::summarize(&prefill_samples),
        decode_samples,
        prefill_samples,
    })
}

/// The refusal the spec pins: two points define a line, and extrapolating
/// from it is how a benchmark invents numbers.
#[must_use]
pub fn curve_note(distinct_depths: usize) -> Option<String> {
    (!stats::can_fit_curve(distinct_depths)).then(|| {
        format!("insufficient depths to fit a curve — measure at least 3 (got {distinct_depths})")
    })
}
```

Register `pub mod sweep;`.

- [ ] **Step 4: Run** `cargo test bench::sweep` → PASS; `make lint`.
- [ ] **Step 5: Commit** `feat(bench): depth sweep with auditable samples and the curve refusal`

---

### Task 7: the store — auditable run records

**Files:**
- Create: `src/core/bench/store.rs` (+ register)
- Modify: `src/error.rs` (`BenchRunInvalid`)

**Interfaces:**
- Consumes: `clock::utc_compact_now`, `sweep::DepthResult`, `grade::Grade`, `stats::summarize`.
- Produces:

```rust
pub struct RunRecord {
    pub schema_version: u32,          // 1
    pub created_utc: String,          // clock::utc_compact format
    pub model: String,
    pub ctx: u32,                     // from /props — what the server LOADED
    pub launch_args: Vec<String>,     // flag hygiene: the exact argv benched
    pub engine_build_commit: Option<String>,
    pub machine: MachineRecord,
    pub depths: Vec<DepthRecord>,
    pub fixture: Vec<ProbeRecord>,    // empty when no --fixture
}
pub struct MachineRecord { pub chip: Option<String>, pub memsize_bytes: Option<u64>, pub gpu_budget_mib: Option<u64>, pub budget_provenance: Option<String> }
pub struct DepthRecord { pub depth: u32, pub prompt_n: u64, pub decode_samples: Vec<f64>, pub prefill_samples: Vec<f64> }
pub struct ProbeRecord { pub id: String, pub pass: bool, pub reason: Option<String> }
pub fn save(dir: &Path, record: &RunRecord) -> Result<PathBuf, ChekovError>   // <dir>/<created_utc>-<model>.json
pub fn load(path: &Path) -> Result<RunRecord, ChekovError>
pub fn render_run(record: &RunRecord) -> String
impl From<&DepthResult> for DepthRecord
```

  Summaries are NOT stored — `render_run` and `compare` recompute them from the samples via `stats::summarize`, so a stored median can never drift from its samples.

- [ ] **Step 1: Write the failing tests**

```rust
use std::path::PathBuf;

use super::{DepthRecord, MachineRecord, RunRecord, load, render_run, save};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("chekov-test-bench-store").join(name);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn record() -> RunRecord {
    RunRecord {
        schema_version: 1,
        created_utc: "20260827T120000Z".into(),
        model: "ornith-1.5-35b-a3b".into(),
        ctx: 131_072,
        launch_args: vec!["-m".into(), "model.gguf".into()],
        engine_build_commit: Some("79aac7d9".into()),
        machine: MachineRecord {
            chip: Some("Apple M3 Ultra".into()),
            memsize_bytes: Some(274_877_906_944),
            gpu_budget_mib: Some(228_065),
            budget_provenance: Some("engine-reported".into()),
        },
        depths: vec![
            DepthRecord {
                depth: 1024,
                prompt_n: 1093,
                decode_samples: vec![19.0, 21.0, 22.0, 22.4],
                prefill_samples: vec![400.0, 450.0, 455.0, 452.0],
            },
            DepthRecord {
                depth: 4096,
                prompt_n: 4210,
                decode_samples: vec![17.0, 18.5, 18.7, 18.6],
                prefill_samples: vec![380.0, 420.0, 425.0, 422.0],
            },
        ],
        fixture: vec![],
    }
}

#[test]
fn a_run_round_trips_through_disk() {
    let dir = scratch("roundtrip");
    let path = save(&dir, &record()).expect("save");
    let loaded = load(&path).expect("load");
    assert_eq!(loaded.model, "ornith-1.5-35b-a3b");
    assert_eq!(loaded.depths.len(), 2);
    assert_eq!(loaded.depths[0].decode_samples, vec![19.0, 21.0, 22.0, 22.4]);
}

#[test]
fn an_unknown_field_in_a_stored_run_is_refused() {
    let dir = scratch("unknown-field");
    let path = dir.join("bad.json");
    std::fs::write(&path, r#"{"schema_version":1,"surprise":true}"#).expect("write");
    assert!(load(&path).is_err(), "deny_unknown_fields");
}

#[test]
fn a_newer_schema_is_refused_rather_than_misread() {
    let dir = scratch("v2");
    let mut newer = record();
    newer.schema_version = 2;
    let path = save(&dir, &newer).expect("save");
    let err = load(&path).expect_err("too new");
    assert!(err.to_string().contains("schema_version"), "{err}");
}

#[test]
fn the_rendering_recomputes_summaries_and_refuses_the_curve_below_three_depths() {
    let rendered = render_run(&record());
    assert!(rendered.contains("ornith-1.5-35b-a3b"));
    assert!(rendered.contains("insufficient depths to fit a curve"), "{rendered}");
    // Median of [21.0, 22.0, 22.4] after the warmup drop — from stats, not storage.
    assert!(rendered.contains("22.0"), "{rendered}");
    assert!(rendered.contains("warmup"), "the drop is visible, never absorbed: {rendered}");
}
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement** `src/core/bench/store.rs`:

```rust
//! One bench run on disk, complete enough that a later `compare` — or a
//! skeptical human — can audit every summary back to its raw samples.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::bench::sweep::{DepthResult, curve_note};
use crate::core::stats;
use crate::error::ChekovError;

const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunRecord {
    pub schema_version: u32,
    pub created_utc: String,
    pub model: String,
    /// What the server LOADED (`/props`), not what the registry intended.
    pub ctx: u32,
    /// Flag hygiene: the exact argv the measured server was launched with.
    pub launch_args: Vec<String>,
    pub engine_build_commit: Option<String>,
    pub machine: MachineRecord,
    pub depths: Vec<DepthRecord>,
    #[serde(default)]
    pub fixture: Vec<ProbeRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineRecord {
    pub chip: Option<String>,
    pub memsize_bytes: Option<u64>,
    pub gpu_budget_mib: Option<u64>,
    pub budget_provenance: Option<String>,
}

/// Raw samples only — summaries are recomputed on read so a stored median can
/// never drift from what was measured.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DepthRecord {
    pub depth: u32,
    pub prompt_n: u64,
    pub decode_samples: Vec<f64>,
    pub prefill_samples: Vec<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeRecord {
    pub id: String,
    pub pass: bool,
    pub reason: Option<String>,
}

impl From<&DepthResult> for DepthRecord {
    fn from(result: &DepthResult) -> Self {
        Self {
            depth: result.depth,
            prompt_n: result.prompt_n,
            decode_samples: result.decode_samples.clone(),
            prefill_samples: result.prefill_samples.clone(),
        }
    }
}

pub fn save(dir: &Path, record: &RunRecord) -> Result<PathBuf, ChekovError> {
    std::fs::create_dir_all(dir)
        .map_err(|e| ChekovError::io(format!("creating {}", dir.display()), e))?;
    let path = dir.join(format!("{}-{}.json", record.created_utc, record.model));
    let json = serde_json::to_string_pretty(record).map_err(|e| ChekovError::BenchRunInvalid {
        path: path.clone(),
        reason: e.to_string(),
    })?;
    std::fs::write(&path, json)
        .map_err(|e| ChekovError::io(format!("writing {}", path.display()), e))?;
    Ok(path)
}

pub fn load(path: &Path) -> Result<RunRecord, ChekovError> {
    let invalid = |reason: String| ChekovError::BenchRunInvalid {
        path: path.to_path_buf(),
        reason,
    };
    let text = std::fs::read_to_string(path).map_err(|e| invalid(e.to_string()))?;
    let record: RunRecord = serde_json::from_str(&text).map_err(|e| invalid(e.to_string()))?;
    if record.schema_version != SCHEMA_VERSION {
        return Err(invalid(format!(
            "schema_version {} — this chekov reads {SCHEMA_VERSION}",
            record.schema_version
        )));
    }
    Ok(record)
}

/// The run as a table, summaries recomputed from the samples.
#[must_use]
pub fn render_run(record: &RunRecord) -> String {
    let mut out = header_line(record);
    out.push_str("depth  prompt_n  decode tok/s (median [p10..p90])  prefill tok/s  n\n");
    for depth in &record.depths {
        out.push_str(&depth_line(depth));
    }
    if let Some(note) = curve_note(summarisable_depths(record)) {
        out.push_str(&note);
        out.push('\n');
    }
    for probe in &record.fixture {
        let verdict = if probe.pass { "PASS" } else { "FAIL" };
        let reason = probe.reason.as_deref().unwrap_or("");
        out.push_str(&format!("fixture {verdict} {}  {reason}\n", probe.id));
    }
    out
}

fn header_line(record: &RunRecord) -> String {
    let engine = record.engine_build_commit.as_deref().unwrap_or("unknown");
    format!(
        "bench {}  ctx {}  engine {engine}  {}\n",
        record.model, record.ctx, record.created_utc
    )
}

fn depth_line(depth: &DepthRecord) -> String {
    let decode = stats::summarize(&depth.decode_samples);
    let prefill = stats::summarize(&depth.prefill_samples);
    match (decode, prefill) {
        (Some(d), Some(p)) => format!(
            "{:>5}  {:>8}  {:.1} [{:.1}..{:.1}]  {:.1}  {} ({} warmup dropped)\n",
            depth.depth, depth.prompt_n, d.median, d.p10, d.p90, p.median, d.n, d.warmup_dropped
        ),
        _ => format!(
            "{:>5}  {:>8}  too few samples to summarise\n",
            depth.depth, depth.prompt_n
        ),
    }
}

fn summarisable_depths(record: &RunRecord) -> usize {
    record
        .depths
        .iter()
        .filter(|d| stats::summarize(&d.decode_samples).is_some())
        .count()
}
```

`src/error.rs`:

```rust
#[error("bench run {}: {reason}", path.display())]
BenchRunInvalid { path: PathBuf, reason: String },
```

Register `pub mod store;`.

- [ ] **Step 4: Run** `cargo test bench::store` → PASS; `make lint`.
- [ ] **Step 5: Commit** `feat(bench): auditable run records — raw samples stored, summaries recomputed`

---

### Task 8: compare — same-engine only

**Files:**
- Create: `src/core/bench/compare.rs` (+ register)
- Modify: `src/error.rs` (`BenchEngineMismatch`)

**Interfaces:**
- Consumes: `store::RunRecord`, `stats::{summarize, compare, Comparison, Summary}`.
- Produces:
  - `pub struct DepthComparison { pub depth: u32, pub a: Summary, pub b: Summary, pub verdict: Comparison }`
  - `pub fn compare_runs(a: &RunRecord, b: &RunRecord, significance_pct: f64) -> Result<Vec<DepthComparison>, ChekovError>`
  - `pub struct RunPair<'a> { pub a: &'a RunRecord, pub b: &'a RunRecord }`
  - `pub fn render_comparison(pair: &RunPair, rows: &[DepthComparison]) -> String` — `NoSignificantDifference` renders the literal words `no significant difference`.

- [ ] **Step 1: Write the failing tests**

```rust
use super::{RunPair, compare_runs, render_comparison};
use crate::core::bench::store::{DepthRecord, MachineRecord, RunRecord};
use crate::core::stats::Comparison;
use crate::error::ChekovError;

fn run(model: &str, engine: Option<&str>, decode: &[f64]) -> RunRecord {
    RunRecord {
        schema_version: 1,
        created_utc: "20260827T120000Z".into(),
        model: model.into(),
        ctx: 131_072,
        launch_args: vec![],
        engine_build_commit: engine.map(str::to_owned),
        machine: MachineRecord {
            chip: None,
            memsize_bytes: None,
            gpu_budget_mib: None,
            budget_provenance: None,
        },
        depths: vec![DepthRecord {
            depth: 1024,
            prompt_n: 1000,
            decode_samples: decode.to_vec(),
            prefill_samples: decode.to_vec(),
        }],
        fixture: vec![],
    }
}

#[test]
fn differing_engines_are_refused_naming_the_field() {
    let a = run("m1", Some("79aac7d9"), &[19.0, 21.0, 22.0]);
    let b = run("m2", Some("00c0ffee"), &[19.0, 21.0, 22.0]);
    let err = compare_runs(&a, &b, 5.0).expect_err("cross-engine");
    assert!(matches!(err, ChekovError::BenchEngineMismatch { .. }));
    assert!(err.to_string().contains("engine.build_commit"), "{err}");
}

#[test]
fn an_unknown_engine_on_either_side_is_also_refused() {
    // "Same engine" cannot be attested when one side never recorded it.
    let a = run("m1", None, &[19.0, 21.0, 22.0]);
    let b = run("m2", Some("79aac7d9"), &[19.0, 21.0, 22.0]);
    assert!(compare_runs(&a, &b, 5.0).is_err());
}

#[test]
fn overlapping_intervals_print_no_significant_difference() {
    let a = run("m1", Some("79aac7d9"), &[19.0, 20.0, 21.0, 22.0]);
    let b = run("m2", Some("79aac7d9"), &[19.5, 20.5, 21.0, 21.5]);
    let rows = compare_runs(&a, &b, 5.0).expect("same engine");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].verdict, Comparison::NoSignificantDifference);
    let rendered = render_comparison(&RunPair { a: &a, b: &b }, &rows);
    assert!(rendered.contains("no significant difference"), "{rendered}");
}

#[test]
fn only_depths_present_in_both_runs_are_compared() {
    let mut a = run("m1", Some("79aac7d9"), &[19.0, 21.0, 22.0]);
    a.depths.push(DepthRecord {
        depth: 4096,
        prompt_n: 4100,
        decode_samples: vec![15.0, 16.0, 17.0],
        prefill_samples: vec![15.0, 16.0, 17.0],
    });
    let b = run("m2", Some("79aac7d9"), &[30.0, 40.0, 41.0]);
    let rows = compare_runs(&a, &b, 5.0).expect("same engine");
    assert_eq!(rows.len(), 1, "depth 4096 exists only in one run");
    assert_eq!(rows[0].depth, 1024);
}

#[test]
fn a_clear_gap_is_called() {
    let a = run("m1", Some("79aac7d9"), &[38.0, 40.0, 41.0, 40.5]);
    let b = run("m2", Some("79aac7d9"), &[19.0, 20.0, 21.0, 20.5]);
    let rows = compare_runs(&a, &b, 5.0).expect("same engine");
    assert_eq!(rows[0].verdict, Comparison::Faster);
}
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement** `src/core/bench/compare.rs`:

```rust
//! Comparison of two stored runs — same engine only. A cross-engine
//! comparison attributes the engine's change to the model and is refused.

use crate::core::bench::store::RunRecord;
use crate::core::stats::{self, Comparison, Summary};
use crate::error::ChekovError;

pub struct DepthComparison {
    pub depth: u32,
    pub a: Summary,
    pub b: Summary,
    pub verdict: Comparison,
}

pub struct RunPair<'a> {
    pub a: &'a RunRecord,
    pub b: &'a RunRecord,
}

pub fn compare_runs(
    a: &RunRecord,
    b: &RunRecord,
    significance_pct: f64,
) -> Result<Vec<DepthComparison>, ChekovError> {
    assert_same_engine(a, b)?;
    let mut rows = Vec::new();
    for depth_a in &a.depths {
        let Some(depth_b) = b.depths.iter().find(|d| d.depth == depth_a.depth) else {
            continue;
        };
        let (Some(sum_a), Some(sum_b)) = (
            stats::summarize(&depth_a.decode_samples),
            stats::summarize(&depth_b.decode_samples),
        ) else {
            continue;
        };
        let verdict = stats::compare(&sum_a, &sum_b, significance_pct);
        rows.push(DepthComparison { depth: depth_a.depth, a: sum_a, b: sum_b, verdict });
    }
    Ok(rows)
}

/// `engine.build_commit` must be recorded AND equal on both sides — an
/// unrecorded engine cannot be attested to be the same one.
fn assert_same_engine(a: &RunRecord, b: &RunRecord) -> Result<(), ChekovError> {
    match (&a.engine_build_commit, &b.engine_build_commit) {
        (Some(commit_a), Some(commit_b)) if commit_a == commit_b => Ok(()),
        (commit_a, commit_b) => Err(ChekovError::BenchEngineMismatch {
            a: commit_a.clone().unwrap_or_else(|| "unrecorded".to_owned()),
            b: commit_b.clone().unwrap_or_else(|| "unrecorded".to_owned()),
        }),
    }
}

#[must_use]
pub fn render_comparison(pair: &RunPair, rows: &[DepthComparison]) -> String {
    let mut out = format!(
        "compare {} vs {}  (engine {})\n",
        pair.a.model,
        pair.b.model,
        pair.a.engine_build_commit.as_deref().unwrap_or("unrecorded"),
    );
    if rows.is_empty() {
        out.push_str("no depth measured in both runs — nothing to compare\n");
        return out;
    }
    for row in rows {
        out.push_str(&verdict_line(pair, row));
    }
    out
}

fn verdict_line(pair: &RunPair, row: &DepthComparison) -> String {
    let numbers = format!(
        "{:.1} vs {:.1} tok/s (p10-p90 [{:.1}..{:.1}] vs [{:.1}..{:.1}])",
        row.a.median, row.b.median, row.a.p10, row.a.p90, row.b.p10, row.b.p90
    );
    match row.verdict {
        Comparison::Faster => {
            format!("depth {:>6}: {} is faster — {numbers}\n", row.depth, pair.a.model)
        }
        Comparison::Slower => {
            format!("depth {:>6}: {} is faster — {numbers}\n", row.depth, pair.b.model)
        }
        Comparison::NoSignificantDifference => {
            format!("depth {:>6}: no significant difference — {numbers}\n", row.depth)
        }
    }
}
```

`src/error.rs`:

```rust
#[error(
    "these runs were measured on different engines — engine.build_commit is \
     '{a}' vs '{b}' — a cross-engine comparison attributes the engine's change \
     to the model; re-bench on one engine and compare those runs"
)]
BenchEngineMismatch { a: String, b: String },
```

Register `pub mod compare;`.

- [ ] **Step 4: Run** `cargo test bench::compare` → PASS; `make lint`.
- [ ] **Step 5: Commit** `feat(bench): compare refuses cross-engine runs and prints no-significant-difference`

---

### Task 9: CLI wiring — `capability bench` and `capability compare`

**Files:**
- Modify: `src/commands/capability.rs` (two `CapAction` variants + `bench()` / `compare()` + helpers + parse tests)
- Modify: `src/error.rs` (`BenchWrongModel`)
- Modify: `CHANGELOG.md` (Unreleased → Added), `IDEAS.md` (status line of the capability idea)

**Interfaces:**
- Consumes: everything above, plus `server::{live_pid, read_run_state, launch_args}`, `machine::probe`, `engine::{recorded_commit, current_commit}`, `clock::utc_compact_now`, `ClaudeFacade`.

- [ ] **Step 1: Write the failing tests** (capability.rs tests mod; match the file's existing test style)

```rust
#[test]
fn bench_and_compare_parse() {
    use clap::Parser;
    let cli = crate::cli::Cli::try_parse_from([
        "chekov", "capability", "bench", "--fixture", "probes.toml",
    ])
    .expect("bench parses");
    // Reach the variant through the enum; adjust to the Cli struct's field names.
    match cli.cmd {
        crate::cli::Cmd::Capability(cap) => match cap.action {
            Some(super::CapAction::Bench { fixture }) => {
                assert_eq!(fixture.as_deref(), Some(std::path::Path::new("probes.toml")));
            }
            other => panic!("expected Bench, got {other:?}"),
        },
        _ => panic!("expected capability"),
    }
    let cli = crate::cli::Cli::try_parse_from(["chekov", "capability", "compare", "a.json", "b.json"])
        .expect("compare parses");
    match cli.cmd {
        crate::cli::Cmd::Capability(cap) => {
            assert!(matches!(cap.action, Some(super::CapAction::Compare { .. })));
        }
        _ => panic!("expected capability"),
    }
}
```

(If `Cli`'s field is named differently, follow the existing parse tests in `src/cli.rs` — there are precedents for subcommand parse assertions; mirror them rather than inventing a new access pattern.)

- [ ] **Step 2: Run** → FAIL (no variants).

- [ ] **Step 3: Implement**

`CapAction` additions:

```rust
/// Measure the running server through chekov's own translator; store the run.
Bench {
    /// Graded probe set (TOML). There is no compiled-in fixture yet:
    /// fixture-v1 is release-gated on a three-model measurement campaign.
    #[arg(long)]
    fixture: Option<std::path::PathBuf>,
},
/// Compare two stored bench runs (same engine only).
Compare {
    /// Run records written by `capability bench`.
    a: std::path::PathBuf,
    b: std::path::PathBuf,
},
```

Dispatch arms in `CapabilityCmd::run`:

```rust
Some(CapAction::Bench { fixture }) => return bench(ctx, fixture.as_deref()),
Some(CapAction::Compare { a, b }) => return compare(ctx, a, b),
```

`src/error.rs`:

```rust
#[error(
    "the server is running '{running}' but the resolved model is '{resolved}' \
     — bench refuses to record one model's numbers under another's name; \
     `chekov restart` and re-run"
)]
BenchWrongModel { running: String, resolved: String },
```

Command bodies (in capability.rs; each ≤40 LOC via the bundles):

```rust
use crate::core::bench::{compare as bench_compare, fixture, grade, probes, runner, store, sweep};
use crate::core::proxy::claude::ClaudeFacade;
use crate::core::proxy::AgentFacade;

/// The context a bench run needs beyond `Ctx`, resolved and guarded up front.
struct BenchSetup {
    name: String,
    eff: crate::core::registry::Effective,
    pid: i32,
}

fn bench_setup(ctx: &Ctx) -> Result<BenchSetup, ChekovError> {
    let reg = ctx.registry()?;
    let name = reg.active_name()?.to_owned();
    let eff = reg.effective(&name)?;
    let pid = crate::core::server::live_pid(&ctx.config).ok_or(ChekovError::ServerNotRunning)?;
    if let Some(running) = crate::core::server::read_run_state(&ctx.config) {
        if running != name {
            return Err(ChekovError::BenchWrongModel { running, resolved: name });
        }
    }
    Ok(BenchSetup { name, eff, pid })
}

fn bench(ctx: &Ctx, fixture_path: Option<&Path>) -> Result<ExitCode, ChekovError> {
    let setup = bench_setup(ctx)?;
    let cfg = &ctx.config;
    let upstream = runner::UpstreamTarget {
        base_url: cfg.base_url(),
        api_key: cfg.file.server.api_key.clone(),
    };
    let ready = runner::ReadyTarget { base_url: cfg.base_url(), pid: setup.pid };
    runner::wait_ready(ctx.http.as_ref(), &ready, (&cfg.file.bench).into())?;
    let ctx_loaded = runner::assert_props_ctx(ctx.http.as_ref(), &upstream, setup.eff.ctx_size)?;
    let facade = ClaudeFacade::new(&setup.name);
    let wire = runner::ProbeWire { http: ctx.http.as_ref(), facade: &facade, upstream: &upstream };
    let results = sweep::run_sweep(&(&cfg.file.bench).into(), &mut |req| runner::cross(&wire, req))?;
    let graded = fixture_path.map(|p| grade_fixture(&wire, p)).transpose()?.unwrap_or_default();
    let record = build_record(ctx, &setup, BenchOutcome { ctx_loaded, results, graded });
    let path = store::save(&cfg.bench_dir(), &record)?;
    print!("{}", store::render_run(&record));
    println!("stored: {}", path.display());
    Ok(ExitCode::SUCCESS)
}

/// Cross and grade every fixture probe. A crossing failure records a FAIL
/// with its reason — a broken exchange must never look like an empty reply.
fn grade_fixture(
    wire: &runner::ProbeWire,
    path: &Path,
) -> Result<Vec<store::ProbeRecord>, ChekovError> {
    let loaded = fixture::load(path)?;
    let mut out = Vec::new();
    for probe in &loaded.probes {
        let outcome = runner::cross(wire, &probes::fixture_probe(probe))
            .map(|artifact| grade::grade(&artifact.anthropic_body, probe));
        let (pass, reason) = match outcome {
            Ok(grade::Grade::Pass) => (true, None),
            Ok(grade::Grade::Fail { reason }) => (false, Some(reason)),
            Err(e) => (false, Some(e.to_string())),
        };
        out.push(store::ProbeRecord { id: probe.id.clone(), pass, reason });
    }
    Ok(out)
}

/// What one bench measured, bundled for the record builder (§4).
struct BenchOutcome {
    ctx_loaded: u32,
    results: Vec<sweep::DepthResult>,
    graded: Vec<store::ProbeRecord>,
}

fn build_record(ctx: &Ctx, setup: &BenchSetup, outcome: BenchOutcome) -> store::RunRecord {
    let cfg = &ctx.config;
    let machine = crate::core::machine::probe(&cfg.engine_dir());
    store::RunRecord {
        schema_version: 1,
        created_utc: crate::core::clock::utc_compact_now(),
        model: setup.name.clone(),
        ctx: outcome.ctx_loaded,
        launch_args: crate::core::server::launch_args(cfg, &setup.eff),
        engine_build_commit: crate::core::engine::recorded_commit(&cfg.logs_dir())
            .or_else(|| crate::core::engine::current_commit(&cfg.engine_dir())),
        machine: store::MachineRecord {
            chip: machine.chip,
            memsize_bytes: machine.memsize_bytes,
            gpu_budget_mib: machine.budget.map(|b| b.value),
            budget_provenance: machine.budget.map(|b| b.provenance.label().to_owned()),
        },
        depths: outcome.results.iter().map(store::DepthRecord::from).collect(),
        fixture: outcome.graded,
    }
}

fn compare(ctx: &Ctx, a: &Path, b: &Path) -> Result<ExitCode, ChekovError> {
    let run_a = store::load(a)?;
    let run_b = store::load(b)?;
    let rows = bench_compare::compare_runs(
        &run_a,
        &run_b,
        f64::from(ctx.config.file.bench.significance_pct),
    )?;
    print!(
        "{}",
        bench_compare::render_comparison(&bench_compare::RunPair { a: &run_a, b: &run_b }, &rows)
    );
    Ok(ExitCode::SUCCESS)
}
```

(Adjust `schema_version: 1` — expose `pub const SCHEMA_VERSION` from store.rs and use it here instead of a bare literal.)

`CHANGELOG.md` under Unreleased/Added:

```markdown
- `chekov capability bench [--fixture <path>]` — slice 5 of the capability
  spec, completed. Measures the running server THROUGH chekov's own
  Anthropic<->OpenAI translator: /health+pid readiness (a server that dies
  while loading fails as "died", not as a timeout), a /props assertion that
  the loaded n_ctx matches the config's intent, a depth sweep whose samples
  are stored raw (summaries are recomputed, so a stored median can never
  drift), and optional graded probes from a user-supplied TOML fixture.
  There is deliberately no compiled-in fixture: fixture-v1 is release-gated
  on a three-model measurement campaign.
- `chekov capability compare <a.json> <b.json>` — refuses runs from different
  engine builds (naming `engine.build_commit`), and prints
  `no significant difference` as a first-class outcome rather than forcing a
  winner.
```

`IDEAS.md`: update the capability idea's status line to record slice 5's harness as shipped with fixture-v1 still gated (statuses are updated in place; body text stays).

- [ ] **Step 4: Run** `make lint && make test` → all green.
- [ ] **Step 5: Manual exit demonstration** (only if a server is up): `chekov capability bench` against the live model; paste output into the PR. If no server is running, note the pending manual step in the PR body.
- [ ] **Step 6: Commit** `feat(bench): capability bench and compare — slice 5 complete (fixture-v1 still gated)`

---

## Execution deviations (recorded 2026-08-27, all tasks complete)

- **No `HttpClient::get_auth` and no `UpstreamTarget`.** Pushkin's
  `suppression.new` rule blocks every write to `src/core/hub.rs` (its
  pre-existing sanctioned `#[expect]` at line 247 false-fires as "new" on any
  edit; hard-stopped at 3/3 attempts). Verified `/props` is NOT public in the
  vendored engine (only /health, /models + UI assets bypass `--api-key`), so
  auth was required. Resolution: `get_bearer(upstream, path)` lives in
  `src/core/proxy/serve.rs` beside `Bridge::post` — the right facade, since
  hub.rs is the Hugging Face seam — reusing `proxy::serve::Upstream`
  everywhere the plan said `UpstreamTarget`, and `assert_props_ctx` takes a
  `&PropsFetch` closure seam (2 args) instead of `&dyn HttpClient`.
- `bench()` was over the 40-LOC gate by one line; readiness+props moved into
  `ensure_ready(ctx, &upstream, &setup) -> Result<u32>`.
- `render_run`'s fixture lines are built by `probe_line` + collect
  (clippy `format_push_string` gate).
- Live exit demonstration pending: no llama-server was running during
  implementation. Offline demonstrations performed with the real binary:
  bench→`ServerNotRunning`, compare same-engine→`no significant difference`,
  compare cross-engine→refusal naming `engine.build_commit`.

## Self-Review (done at planning time)

- **Spec coverage:** runner (readiness/pid/props/flag-hygiene/timings) → Tasks 2–3; store → 7; sweep → 6; probes → 4; grade + fixture → 5; compare → 8; CLI → 9. `stats.rs` and the translator-crossing acceptance test shipped in PR #27. Acceptance test 2 (`compare` refuses differing `engine.build_commit` naming the field) → Task 8. Acceptance tests 3–4 already green in `core::stats`; the "insufficient depths" WORDING surfaces via Task 6's `curve_note` and Task 7's renderer. The fixture release gate is honored by shipping no compiled-in fixture. The "`--metric tok-s` grid upgrades from predicted to measured" spec line is deliberately deferred: wiring measured medians into `capability graph` touches slice-2 surfaces and reads stored runs; it goes to IDEAS as a follow-up rather than expanding this plan's blast radius.
- **Placeholder scan:** none — every step carries real code.
- **Type consistency:** `UpstreamTarget`/`ReadyTarget`/`ProbeWire`/`ProbeArtifact`/`Timings` defined in Task 2/3 and consumed by 6/9 under the same names; `DepthResult` (Task 6) → `DepthRecord::from` (Task 7); `RunRecord` (Task 7) → `compare_runs` (Task 8) → CLI (Task 9). `significance_pct: u32` in config, widened with `f64::from` at the one call site.
