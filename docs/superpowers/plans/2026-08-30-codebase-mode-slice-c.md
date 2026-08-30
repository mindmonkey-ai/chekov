# Codebase Mode Slice C — `bench --judge` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `capability bench --codebase … --judge <NAME>` runs a registered judge model in its own phase after every candidate is down and appends one position-swapped, grammar-forced, strictly-parsed binary verdict row per `function_body` crossing, reported as an `equiv` cell that is voided below a swap-consistency floor and never blended with the deterministic tiers.

**Architecture:** A new `core/bench/judge.rs` owns everything a verdict IS — the rubric (a prompt-only `judge_rubric.md` pulled in with `include_str!`), its hash, the family key, eligibility, the two Anthropic-shaped requests, the strict reply parse and the swap combination — with no HTTP import; the requests cross the existing forced wire (`runner::cross_forced_with`, which grows one optional `reasoning_effort` field). Verdicts are their own append-only `suite = "judge"` rows keyed by the crossing's task id, so `--resume` skips them like any other row; the judge's identity, rubric hash, budget and floor ride in a `JudgeStamp` inside the stamp. The command layer resolves a `JudgePlan` before any launch (role, family, running-server refusals), adds a judge step to the plan and estimate, and runs `run_judge_phase` (launch once → every run directory → teardown) after `run_candidates`. `store` renders the `equiv` cell, header clause and trailer from the rows and the stamp alone; `compare` adds an `equiv` row when both runs share a judge and a `not compared` line when they do not.

**Tech Stack:** Rust 2024 (≥1.88), clap derive, serde/serde_json/toml, thiserror; llama.cpp `llama-server` via chekov's own Anthropic→OpenAI translator; no new dependencies.

**Spec:** `docs/superpowers/specs/2026-08-30-codebase-mode-slice-c-design.md` (§0 records the research-pass changes; §3.0 the measured probe).

## Global Constraints

- `make lint && make test` green before every commit (`cargo fmt --check && cargo clippy --all-targets -- -D warnings`; `cargo test`). No `unwrap()`/`expect()` outside tests. `#![forbid(unsafe_code)]`.
- Functions ≤ 40 LOC, ≤ 3 parameters (bundle into a struct past that), no boolean flag parameters. clap derive structs are exempt.
- Commit protocol from AGENTS.md: tests first as `test(<module>): red`, then the implementation as `feat(<module>): …`/`fix(…)`. One concern per commit. Every commit ends with the two trailers:
  `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>` and `Claude-Session: https://claude.ai/code/session_01W3c1qNThCPR2kDgazkLe6b`.
- `judge.rs` imports nothing from `proxy::http`/`ureq`; it hands `HttpRequest`s to `runner` and parses the Anthropic body `runner` returns.
- Every new user-facing string in this plan is verbatim from the spec; do not reword.
- Never `cd` in Bash; run everything from the repo root. Never add `role = "judge"` to `models.toml` before Task 1 has landed (the registry is `deny_unknown_fields`).
- Branch: `feat/codebase-mode-c` (exists). PR base: `develop`.

---

### Task 1: `ModelRole` on the registry

**Files:**
- Modify: `src/core/registry.rs:62-79` (`ModelEntry`), tests module at the bottom of the same file

**Interfaces:**
- Produces: `pub enum ModelRole { Judge }` (`Copy`, `Serialize` as `"judge"`, custom `Deserialize` with the spec's message); `ModelEntry.role: Option<ModelRole>`.

- [ ] **Step 1: Write the failing tests** (append inside `mod tests` in `src/core/registry.rs`)

```rust
    #[test]
    fn a_judge_role_round_trips_and_an_absent_one_loads_as_none() {
        let mut entry = sample_entry();
        entry.role = Some(super::ModelRole::Judge);
        let text = toml::to_string(&entry).expect("serialize");
        assert!(text.contains("role = \"judge\""), "{text}");
        let back: super::ModelEntry = toml::from_str(&text).expect("parse");
        assert_eq!(back.role, Some(super::ModelRole::Judge));
        let plain: super::ModelEntry =
            toml::from_str(&toml::to_string(&sample_entry()).expect("serialize")).expect("parse");
        assert_eq!(plain.role, None, "no field means no role, not an error");
    }

    #[test]
    fn an_unknown_role_is_refused_naming_the_one_accepted_value() {
        let text = format!(
            "{}role = \"candidate\"\n",
            toml::to_string(&sample_entry()).expect("serialize")
        );
        let err = toml::from_str::<super::ModelEntry>(&text)
            .expect_err("a role chekov does not know")
            .to_string();
        assert!(
            err.contains("role = \"candidate\" is not a role chekov knows; the one accepted value is \"judge\""),
            "{err}"
        );
    }
```

Also add `role: None,` to `sample_entry()` (the struct literal at `registry.rs:200-210`).

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib registry::tests -- role`
Expected: compile error — `no field role on type ModelEntry` / `ModelRole` not found.

- [ ] **Step 3: Implement**

In `src/core/registry.rs`, before `ModelEntry`:

```rust
/// What a registered model is FOR beyond serving: today only `"judge"`, the
/// role `bench --judge` requires. Parsed at the boundary — nothing downstream
/// compares a string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelRole {
    Judge,
}

impl<'de> Deserialize<'de> for ModelRole {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        match raw.as_str() {
            "judge" => Ok(Self::Judge),
            other => Err(serde::de::Error::custom(format!(
                "role = \"{other}\" is not a role chekov knows; the one accepted value is \"judge\""
            ))),
        }
    }
}
```

Add to `ModelEntry` after `extra_flags`:

```rust
    /// `role = "judge"` marks a model `bench --judge` may use. Absent on every
    /// entry that is only served.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<ModelRole>,
```

Then `cargo build` and add `role: None,` to every `ModelEntry { … }` literal the compiler names (`src/commands/pull.rs`, tests in `src/commands/*.rs`, `src/core/*.rs`).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib registry::tests && make lint`
Expected: PASS; clippy clean.

- [ ] **Step 5: Commit**

```bash
git add src/core/registry.rs src/commands src/core
git commit -m "feat(registry): role = \"judge\" on a model entry, parsed at the boundary"
```

---

### Task 2: The three `[bench]` judge knobs

**Files:**
- Modify: `src/core/config.rs:112-157` (`BenchSection` + `Default`), tests at `config.rs:322-355`

**Interfaces:**
- Produces: `BenchSection.judge_max_tokens: u32` (512), `judge_min_consistency_pct: u32` (70), `judge_reasoning_effort: ReasoningEffort` (`Low`); `pub enum ReasoningEffort { None, Low, Medium, High }` with `as_str()` returning llama.cpp's spelling (`"none"`, `"low"`, `"medium"`, `"high"`).

- [ ] **Step 1: Write the failing test** (in `config.rs` tests)

```rust
    #[test]
    fn the_judge_knobs_default_and_parse() {
        let cfg: super::FileConfig = toml::from_str("").expect("defaults");
        assert_eq!(cfg.bench.judge_max_tokens, 512);
        assert_eq!(cfg.bench.judge_min_consistency_pct, 70);
        assert_eq!(cfg.bench.judge_reasoning_effort, super::ReasoningEffort::Low);
        assert_eq!(cfg.bench.judge_reasoning_effort.as_str(), "low");
        let cfg: super::FileConfig = toml::from_str(
            "[bench]\njudge_max_tokens = 64\njudge_min_consistency_pct = 80\njudge_reasoning_effort = \"none\"\n",
        )
        .expect("overrides parse");
        assert_eq!(cfg.bench.judge_max_tokens, 64);
        assert_eq!(cfg.bench.judge_min_consistency_pct, 80);
        assert_eq!(cfg.bench.judge_reasoning_effort, super::ReasoningEffort::None);
        assert!(
            toml::from_str::<super::FileConfig>("[bench]\njudge_reasoning_effort = \"max\"\n").is_err(),
            "an effort llama.cpp does not spell is refused at load"
        );
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib config::tests::the_judge_knobs_default_and_parse`
Expected: compile error (missing fields / type).

- [ ] **Step 3: Implement** — in `config.rs` above `BenchSection`:

```rust
/// llama.cpp's `reasoning_effort` spellings, as the judge wire sends them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    None,
    Low,
    Medium,
    High,
}

impl ReasoningEffort {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}
```

Fields appended to `BenchSection`:

```rust
    /// `max_tokens` on every judge request. 512 is twice the longest reply
    /// the 2026-08-30 probe saw from a thinking judge; a non-thinking judge
    /// stops at ~8 tokens regardless.
    pub judge_max_tokens: u32,
    /// Below this swap-agreement rate the `equiv` column is voided, never
    /// down-weighted (spec §10).
    pub judge_min_consistency_pct: u32,
    /// `reasoning_effort` on every judge request — gpt-oss needs it, Gemma's
    /// template ignores it.
    pub judge_reasoning_effort: ReasoningEffort,
```

Defaults: `judge_max_tokens: 512, judge_min_consistency_pct: 70, judge_reasoning_effort: ReasoningEffort::Low,`.

- [ ] **Step 4: Run to verify it passes** — `cargo test --lib config::tests && make lint`

- [ ] **Step 5: Commit** — `git commit -m "feat(config): judge_max_tokens, judge_min_consistency_pct, judge_reasoning_effort"`

---

### Task 3: `reasoning_effort` on the forced wire, judge requests only

**Files:**
- Modify: `src/core/bench/runner.rs:161-169` (`cross_forced`), `:183-201` (`cross_inner`), `:402-433` (`adjust_body`); tests near `:1152`

**Interfaces:**
- Produces: `pub struct Forced<'a> { pub schema: &'a Value, pub reasoning_effort: Option<&'a str> }`; `pub fn cross_forced_with(wire: &ProbeWire, req: &HttpRequest, forced: &Forced) -> Result<ProbeArtifact, ChekovError>`. `cross_forced(wire, req, schema)` keeps its signature and sends no `reasoning_effort`.

- [ ] **Step 1: Write the failing test** (in runner's `mod tests`, beside `the_forced_wire_carries_response_format_beside_the_pins`)

```rust
    #[test]
    fn only_a_judge_crossing_carries_reasoning_effort() {
        let facade = ClaudeFacade::new("local-model");
        let up = fake_upstream();
        let schema = serde_json::json!({"type": "object"});
        let judge = CannedUpstream::new(openai_with_timings());
        super::cross_forced_with(
            &wire(&judge, &facade, &up),
            &anthropic_request("judge it"),
            &super::Forced { schema: &schema, reasoning_effort: Some("low") },
        )
        .expect("judge crossing");
        assert_eq!(sent(&judge)["reasoning_effort"], "low", "{}", sent(&judge));
        assert_eq!(sent(&judge)["response_format"]["type"], "json_schema");
        assert_eq!(sent(&judge)["reasoning_format"], "deepseek");

        let probe = CannedUpstream::new(openai_with_timings());
        super::cross_forced(&wire(&probe, &facade, &up), &anthropic_request("go"), &schema)
            .expect("forced probe");
        assert!(sent(&probe).get("reasoning_effort").is_none(), "{}", sent(&probe));
    }
```

- [ ] **Step 2: Run to verify it fails** — `cargo test --lib bench::runner::tests::only_a_judge_crossing_carries_reasoning_effort` → compile error.

- [ ] **Step 3: Implement**

```rust
/// What a forced crossing constrains: the grammar, and — on the judge wire
/// only — the engine's reasoning effort. Candidate probes never set the latter.
pub struct Forced<'a> {
    pub schema: &'a Value,
    pub reasoning_effort: Option<&'a str>,
}

pub fn cross_forced(wire: &ProbeWire, req: &HttpRequest, schema: &Value) -> Result<ProbeArtifact, ChekovError> {
    cross_inner(wire, req, Some(&Forced { schema, reasoning_effort: None }))
}

/// `cross_forced` with the judge's extra field (spec C §3.0: one uniform judge wire).
pub fn cross_forced_with(wire: &ProbeWire, req: &HttpRequest, forced: &Forced) -> Result<ProbeArtifact, ChekovError> {
    cross_inner(wire, req, Some(forced))
}
```

`cross_inner` takes `forced: Option<&Forced>`; `adjust_body(body, pins, forced: Option<&Forced>)` inserts `response_format` from `forced.schema` and `reasoning_format` as today, and additionally:

```rust
        if let Some(effort) = f.reasoning_effort {
            object.insert("reasoning_effort".to_owned(), Value::from(effort));
        }
```

(`adjust_body` is at 32 lines today; the two extra lines keep it under 40.)

- [ ] **Step 4: Run** — `cargo test --lib bench::runner && make lint` → PASS.

- [ ] **Step 5: Commit** — `git commit -m "feat(runner): cross_forced_with — reasoning_effort on the judge wire only"`

---

### Task 4: `judge.rs` — rubric, hash, family key, eligibility, requests, reply parse, swap

**Files:**
- Create: `src/core/bench/judge.rs`, `src/core/bench/judge_rubric.md`
- Modify: `src/core/bench/mod.rs` (add `pub mod judge;`), `src/core/bench/codebase/ladder.rs:295` (`pub(super) fn trimmed_to_gold` → `pub(crate)`)

**Interfaces:**
- Consumes: `store::CodebaseRow` (Task 5 adds nothing this task reads), `store::{DecidedBy, JudgeRow}` (Task 5 — write Task 5's types first if executing out of order; they are pure data), `probes::anthropic_post` (already `pub(crate)`), `codebase::ladder::trimmed_to_gold`, `hash::sha256_hex`, `ExecScore`.
- Produces (all `pub`): `RUBRIC`, `CONTEXT_BEFORE_LINES = 40`, `CONTEXT_AFTER_LINES = 20`, `SPAN_MAX_CHARS = 4096`, `fn schema() -> Value`, `fn rubric_hash() -> String` (12 hex chars), `fn family_key(arch: &str) -> &str`, `enum Eligibility<'a> { Identical, Skipped(&'static str), Judge(Pair<'a>) }`, `fn eligibility(row: &CodebaseRow) -> Option<Eligibility>` (`None` = not a judge row at all), `struct Pair<'a> { file, before, after, gold, prediction: &'a str / String }`, `fn requests(pair: &Pair, max_tokens: u32) -> [HttpRequest; 2]` (gold-first, prediction-first), `enum Reply { Answer(bool), Skipped(String) }`, `fn parse_reply(anthropic_body: &str, max_tokens: u32) -> Reply`, `struct Verdict { equivalent: Option<bool>, decided_by: DecidedBy, skipped: Option<String> }`, `fn combine(gold_first: &Reply, prediction_first: &Reply) -> Verdict`, `fn consistency_pct(rows: &[&JudgeRow]) -> Option<u32>`.

- [ ] **Step 1: Write `judge_rubric.md`** (the prompt and nothing else; placeholders are `{{file}}`, `{{before}}`, `{{after}}`, `{{a}}`, `{{b}}`)

````markdown
A and B are two versions of the same span of Rust code, at the same position in the same file. Decide whether B would behave the same as A for every input, in this file, at this position. Reply with the JSON object only: {"same_behavior": true} or {"same_behavior": false}.

File: {{file}}

Lines before the span:
```rust
{{before}}
```

Lines after the span:
```rust
{{after}}
```

A:
```rust
{{a}}
```

B:
```rust
{{b}}
```
````

- [ ] **Step 2: Write the failing tests** (`mod tests` at the bottom of `judge.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::{Eligibility, Reply, combine, consistency_pct, eligibility, family_key, parse_reply, requests, rubric_hash};
    use crate::core::bench::codebase::TaskTier;
    use crate::core::bench::store::{CodebaseRow, DecidedBy, ExecRow, ExecScore, JudgeRow};

    fn row(tier: TaskTier, gold: &str, prediction: &str) -> CodebaseRow {
        CodebaseRow {
            tier,
            file: "src/lib.rs".into(),
            line: 10,
            label: "<mask>".into(),
            gold: gold.into(),
            prediction: prediction.into(),
            prefix: (1..=60).map(|i| format!("before {i}\n")).collect(),
            suffix: (1..=30).map(|i| format!("after {i}\n")).collect(),
            excluded: crate::core::bench::codebase::Excluded::default(),
            symbols_score: Some(1.0),
            unsupported: false,
            arm: None,
            extra: None,
            also_first_uses: Vec::new(),
            name: None,
            n_predict: Some(16),
            exec: None,
        }
    }

    fn body(text: &str, stop: &str) -> String {
        serde_json::json!({
            "content": [{"type": "text", "text": text}],
            "stop_reason": stop,
        })
        .to_string()
    }

    #[test]
    fn the_rubric_hash_is_twelve_hex_chars_and_stable() {
        let h = rubric_hash();
        assert_eq!(h.len(), 12, "{h}");
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(h, rubric_hash());
    }

    #[test]
    fn a_family_is_the_architecture_without_its_moe_suffix() {
        assert_eq!(family_key("qwen35moe"), "qwen35");
        assert_eq!(family_key("qwen35"), "qwen35");
        assert_ne!(family_key("qwen4exp"), family_key("qwen35"));
        assert_eq!(family_key("gpt-oss"), "gpt-oss");
    }

    #[test]
    fn only_answered_function_bodies_are_judged() {
        assert!(eligibility(&row(TaskTier::InFile, "a", "b")).is_none());
        assert!(eligibility(&row(TaskTier::FunctionBody, "a", "")).is_none(), "nobody answered");
        assert!(matches!(eligibility(&row(TaskTier::FunctionBody, "x = 1;", "x = 1;\nextra")), Some(Eligibility::Identical)), "identical after the tiers-1-4 trim");
        let mut failed = row(TaskTier::FunctionBody, "x = 1;", "y = 2;");
        failed.exec = Some(ExecRow { compile: ExecScore::Value(0.0), ..ExecRow::skipped("") });
        assert!(matches!(eligibility(&failed), Some(Eligibility::Skipped("did not compile"))));
        let mut passed = row(TaskTier::FunctionBody, "x = 1;", "y = 2;");
        passed.exec = Some(ExecRow { compile: ExecScore::Value(1.0), ..ExecRow::skipped("") });
        assert!(matches!(eligibility(&passed), Some(Eligibility::Judge(_))));
        assert!(matches!(eligibility(&row(TaskTier::FunctionBody, "x = 1;", "y = 2;")), Some(Eligibility::Judge(_))), "no exec means no compile verdict to defer to");
    }

    #[test]
    fn the_two_requests_swap_a_and_b_and_bound_the_context() {
        let Some(Eligibility::Judge(pair)) = eligibility(&row(TaskTier::FunctionBody, "GOLD;", "PRED;")) else {
            panic!("eligible");
        };
        let [first, second] = requests(&pair, 512);
        let text = |r: &crate::core::proxy::http::HttpRequest| {
            let v: serde_json::Value = serde_json::from_slice(&r.body).expect("json");
            assert_eq!(v["max_tokens"], 512);
            assert_eq!(v["messages"].as_array().map(Vec::len), Some(1), "one user turn: Gemma has no system role");
            v["messages"][0]["content"].as_str().expect("text").to_owned()
        };
        let (t1, t2) = (text(&first), text(&second));
        assert!(t1.find("A:\n```rust\nGOLD;").is_some() && t1.find("B:\n```rust\nPRED;").is_some(), "{t1}");
        assert!(t2.find("A:\n```rust\nPRED;").is_some() && t2.find("B:\n```rust\nGOLD;").is_some(), "{t2}");
        assert!(t1.contains("before 21\n") && !t1.contains("before 20\n"), "last 40 prefix lines: {t1}");
        assert!(t1.contains("after 20\n") && !t1.contains("after 21\n"), "first 20 suffix lines: {t1}");
        assert!(t1.contains("File: src/lib.rs"));
    }

    #[test]
    fn both_spans_are_cut_at_the_same_cap() {
        let long = "x".repeat(super::SPAN_MAX_CHARS + 50);
        let Some(Eligibility::Judge(pair)) = eligibility(&row(TaskTier::FunctionBody, &long, "y")) else { panic!() };
        assert_eq!(pair.gold.len(), super::SPAN_MAX_CHARS);
    }

    #[test]
    fn a_reply_is_parsed_strictly_or_skipped_with_the_reason() {
        assert!(matches!(parse_reply(&body("{\"same_behavior\":true}", "end_turn"), 512), Reply::Answer(true)));
        assert!(matches!(parse_reply(&body("{\"same_behavior\": false}", "end_turn"), 512), Reply::Answer(false)));
        for bad in ["yes", "```json\n{\"same_behavior\":true}\n```", "{\"same_behavior\":true,\"why\":\"x\"}", ""] {
            match parse_reply(&body(bad, "end_turn"), 512) {
                Reply::Skipped(reason) => assert!(reason.starts_with("reply was not the schema: "), "{reason}"),
                Reply::Answer(_) => panic!("{bad:?} must not parse"),
            }
        }
        match parse_reply(&body("{\"same_behavior\":true}", "max_tokens"), 512) {
            Reply::Skipped(reason) => assert_eq!(reason, "reply truncated at 512 tokens"),
            Reply::Answer(_) => panic!("a cut-off reply is not a verdict"),
        }
        let with_thinking = serde_json::json!({
            "content": [{"type": "thinking", "thinking": "hmm", "signature": ""}, {"type": "text", "text": "{\"same_behavior\":false}"}],
            "stop_reason": "end_turn",
        })
        .to_string();
        assert!(matches!(parse_reply(&with_thinking, 512), Reply::Answer(false)), "reasoning beside a valid answer is ignored");
    }

    #[test]
    fn agreement_is_the_verdict_and_disagreement_an_abstention() {
        let v = combine(&Reply::Answer(true), &Reply::Answer(true));
        assert_eq!((v.equivalent, v.decided_by, v.skipped), (Some(true), DecidedBy::SwapAgreement, None));
        let v = combine(&Reply::Answer(true), &Reply::Answer(false));
        assert_eq!((v.equivalent, v.decided_by), (None, DecidedBy::SwapDisagreement));
        let v = combine(&Reply::Answer(true), &Reply::Skipped("reply truncated at 512 tokens".into()));
        assert_eq!((v.equivalent, v.decided_by), (None, DecidedBy::Skipped));
        assert_eq!(v.skipped.as_deref(), Some("reply truncated at 512 tokens"));
    }

    fn judged(gold_first: Option<bool>, prediction_first: Option<bool>) -> JudgeRow {
        JudgeRow {
            equivalent: match (gold_first, prediction_first) {
                (Some(a), Some(b)) if a == b => Some(a),
                _ => None,
            },
            gold_first,
            prediction_first,
            decided_by: DecidedBy::SwapAgreement,
            skipped: None,
            judge_secs: 1.0,
        }
    }

    #[test]
    fn consistency_counts_only_crossings_both_orders_answered() {
        let rows = [judged(Some(true), Some(true)), judged(Some(false), Some(true)), judged(Some(false), None), judged(Some(false), Some(false))];
        let refs: Vec<&JudgeRow> = rows.iter().collect();
        assert_eq!(consistency_pct(&refs), Some(67), "2 agreements of 3 answered pairs");
        assert_eq!(consistency_pct(&[]), None);
        let one = [judged(None, None)];
        assert_eq!(consistency_pct(&[&one[0]]), None, "nothing answered twice: no rate, not 0%");
    }
}
```

- [ ] **Step 3: Run to verify they fail** — `cargo test --lib bench::judge` → module not found.

- [ ] **Step 4: Implement `judge.rs`**

```rust
//! The position-swapped binary judge (spec C §4): what one verdict IS —
//! rubric, eligibility, the request pair, the strict reply parse and the
//! swap rule. No HTTP here: requests go out through `runner`, and what comes
//! back is the Anthropic body `runner` already hands every probe.

use serde::Deserialize;
use serde_json::Value;

use crate::core::bench::codebase::ladder::trimmed_to_gold;
use crate::core::bench::store::{CodebaseRow, DecidedBy, ExecScore, JudgeRow};
use crate::core::bench::codebase::TaskTier;
use crate::core::proxy::http::HttpRequest;

/// The prompt, and nothing else — a file so it diffs as text (§16.10).
pub const RUBRIC: &str = include_str!("judge_rubric.md");
pub const CONTEXT_BEFORE_LINES: usize = 40;
pub const CONTEXT_AFTER_LINES: usize = 20;
pub const SPAN_MAX_CHARS: usize = 4096;
const RUBRIC_VERSION: &str = "judge-v1";

/// The grammar every judge request asks for, and the shape `parse_reply` checks.
#[must_use]
pub fn schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {"same_behavior": {"type": "boolean"}},
        "required": ["same_behavior"],
        "additionalProperties": false,
    })
}

/// `sha256(file bytes ‖ schema ‖ the three constants ‖ "judge-v1")[..12]`.
#[must_use]
pub fn rubric_hash() -> String {
    let canonical = format!(
        "{RUBRIC}|{}|{CONTEXT_BEFORE_LINES}|{CONTEXT_AFTER_LINES}|{SPAN_MAX_CHARS}|{RUBRIC_VERSION}",
        schema()
    );
    crate::core::hash::sha256_hex(canonical.as_bytes())[..12].to_owned()
}

/// `general.architecture` with a trailing `moe` removed: `qwen35moe` and
/// `qwen35` are one family. A floor against sibling preference, not a proof
/// of independence (spec C §2.1).
#[must_use]
pub fn family_key(arch: &str) -> &str {
    arch.strip_suffix("moe").unwrap_or(arch)
}

/// Whether a stored row gets a judge row, and which kind.
pub enum Eligibility<'a> {
    Identical,
    Skipped(&'static str),
    Judge(Pair<'a>),
}

/// The two spans and their bounded context, as the rubric shows them.
pub struct Pair<'a> {
    pub file: &'a str,
    pub before: String,
    pub after: String,
    pub gold: String,
    pub prediction: String,
}

/// `None` is "not a judge row at all": another tier, or a crossing nobody
/// answered. A compile failure is decided already and is skipped, never
/// re-opened.
#[must_use]
pub fn eligibility(row: &CodebaseRow) -> Option<Eligibility<'_>> {
    if row.tier != TaskTier::FunctionBody || row.prediction.is_empty() {
        return None;
    }
    if let Some(exec) = &row.exec
        && exec.compile == ExecScore::Value(0.0)
    {
        return Some(Eligibility::Skipped("did not compile"));
    }
    let prediction = trimmed_to_gold(&row.gold, &row.prediction);
    if row.gold.trim() == prediction.trim() {
        return Some(Eligibility::Identical);
    }
    Some(Eligibility::Judge(Pair {
        file: &row.file,
        before: last_lines(&row.prefix, CONTEXT_BEFORE_LINES),
        after: first_lines(&row.suffix, CONTEXT_AFTER_LINES),
        gold: cap(&row.gold),
        prediction: cap(&prediction),
    }))
}

fn last_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

fn first_lines(text: &str, n: usize) -> String {
    text.lines().take(n).collect::<Vec<_>>().join("\n")
}

fn cap(span: &str) -> String {
    span.char_indices()
        .nth(SPAN_MAX_CHARS)
        .map_or_else(|| span.to_owned(), |(at, _)| span[..at].to_owned())
}

/// Gold-first, then prediction-first: the same bytes with A and B swapped.
#[must_use]
pub fn requests(pair: &Pair, max_tokens: u32) -> [HttpRequest; 2] {
    let ask = |a: &str, b: &str| {
        crate::core::bench::probes::anthropic_post(&serde_json::json!({
            "model": "claude-sonnet-4",
            "max_tokens": max_tokens,
            "messages": [{"role": "user", "content": render(pair, a, b)}],
        }))
    };
    [ask(&pair.gold, &pair.prediction), ask(&pair.prediction, &pair.gold)]
}

fn render(pair: &Pair, a: &str, b: &str) -> String {
    RUBRIC
        .replace("{{file}}", pair.file)
        .replace("{{before}}", &pair.before)
        .replace("{{after}}", &pair.after)
        .replace("{{a}}", a)
        .replace("{{b}}", b)
}

/// One order's outcome: the parsed answer, or why there is none.
pub enum Reply {
    Answer(bool),
    Skipped(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JudgeAnswer {
    same_behavior: bool,
}

/// The schema is asked for on the wire AND checked on the way back — a
/// grammar can be silently inactive (llama.cpp #20345), and a cut-off reply
/// is read from `stop_reason`, the one place the engine says so.
#[must_use]
pub fn parse_reply(anthropic_body: &str, max_tokens: u32) -> Reply {
    let Ok(body) = serde_json::from_str::<Value>(anthropic_body) else {
        return Reply::Skipped("reply was not the schema: <unreadable body>".to_owned());
    };
    if body["stop_reason"] == "max_tokens" {
        return Reply::Skipped(format!("reply truncated at {max_tokens} tokens"));
    }
    let text = body["content"]
        .as_array()
        .and_then(|blocks| blocks.iter().find(|b| b["type"] == "text"))
        .and_then(|b| b["text"].as_str())
        .unwrap_or_default();
    match serde_json::from_str::<JudgeAnswer>(text) {
        Ok(answer) => Reply::Answer(answer.same_behavior),
        Err(_) => Reply::Skipped(format!(
            "reply was not the schema: {}",
            text.chars().take(80).collect::<String>()
        )),
    }
}

/// What the two orders settle to.
pub struct Verdict {
    pub equivalent: Option<bool>,
    pub decided_by: DecidedBy,
    pub skipped: Option<String>,
}

/// Agreement is the verdict; disagreement is an abstention; a skipped order
/// skips the crossing with its reason (the first one, in order).
#[must_use]
pub fn combine(gold_first: &Reply, prediction_first: &Reply) -> Verdict {
    match (gold_first, prediction_first) {
        (Reply::Answer(a), Reply::Answer(b)) if a == b => Verdict {
            equivalent: Some(*a),
            decided_by: DecidedBy::SwapAgreement,
            skipped: None,
        },
        (Reply::Answer(_), Reply::Answer(_)) => Verdict {
            equivalent: None,
            decided_by: DecidedBy::SwapDisagreement,
            skipped: None,
        },
        (Reply::Skipped(reason), _) | (_, Reply::Skipped(reason)) => Verdict {
            equivalent: None,
            decided_by: DecidedBy::Skipped,
            skipped: Some(reason.clone()),
        },
    }
}

/// `agreements / crossings both orders answered`, rounded; `None` when no
/// crossing was answered twice — a rate over nothing is not 0 %.
#[must_use]
pub fn consistency_pct(rows: &[&JudgeRow]) -> Option<u32> {
    let answered: Vec<&&JudgeRow> = rows
        .iter()
        .filter(|r| r.gold_first.is_some() && r.prediction_first.is_some())
        .collect();
    if answered.is_empty() {
        return None;
    }
    let agreed = answered.iter().filter(|r| r.gold_first == r.prediction_first).count();
    Some(u32::try_from((agreed * 100 + answered.len() / 2) / answered.len()).unwrap_or(0))
}
```

Register `pub mod judge;` in `src/core/bench/mod.rs`; make `trimmed_to_gold` `pub(crate)` in `ladder.rs:295`. `Excluded` needs `#[derive(Default)]` if it lacks one (check `codebase/mod.rs`; add the derive). `parse_reply` is 20 lines; `eligibility` 22.

- [ ] **Step 5: Run** — `cargo test --lib bench::judge && make lint` → PASS (Task 5's types must exist; if executing strictly in order, do Task 5 Step 3's data types first — they are pure data — then return here).

- [ ] **Step 6: Commit**

```bash
git add src/core/bench/judge.rs src/core/bench/judge_rubric.md src/core/bench/mod.rs src/core/bench/codebase/ladder.rs
git commit -m "feat(judge): the rubric, its hash, eligibility, the swapped request pair and the strict reply parse"
```

---

### Task 5: `JudgeRow` in the store

**Files:**
- Modify: `src/core/bench/store.rs:55-76` (`TaskRow`), `:138-150` (beside `ExecScore`), `:239-248` (`Task`), `:303-325` (`append`); tests at the bottom of `store.rs`; every `store::Task { … }` and `TaskRow { … }` literal (`cargo build` lists them: `src/commands/capability.rs`, `src/core/bench/codebase/run.rs`, `src/core/bench/speeds.rs` tests, `store.rs` tests)

**Interfaces:**
- Produces: `pub enum DecidedBy { SwapAgreement, SwapDisagreement, Identical, Skipped }` (`snake_case`), `pub struct JudgeRow { equivalent: Option<bool>, gold_first: Option<bool>, prediction_first: Option<bool>, decided_by: DecidedBy, skipped: Option<String>, judge_secs: f64 }`, `TaskRow.judge: Option<JudgeRow>`, `Task.judge: Option<JudgeRow>`, `pub const JUDGE_SUITE: &str = "judge"`, `pub(crate) fn judge_rows(log: &RunLog) -> Vec<&TaskRow>`.

- [ ] **Step 1: Write the failing tests** (store's `mod tests`; `head()`, `scratch()` and `graded()` helpers already exist there)

```rust
    #[test]
    fn a_judge_row_round_trips_and_an_old_row_loads_without_one() {
        let eval = scratch("judge-row");
        let mut writer = RunWriter::create(&eval, "r-judge", &head()).expect("create");
        writer
            .append(Task {
                suite: super::JUDGE_SUITE.into(),
                task_id: "function_body-abc123-L10".into(),
                measure: crate::core::bench::codebase::run::empty_measure(),
                grade: None,
                transport: Transport::Buffered,
                codebase: None,
                judge: Some(JudgeRow {
                    equivalent: None,
                    gold_first: Some(true),
                    prediction_first: Some(false),
                    decided_by: DecidedBy::SwapDisagreement,
                    skipped: None,
                    judge_secs: 2.5,
                }),
            })
            .expect("append");
        let log = RunLog::load(&eval.join("r-judge")).expect("load");
        let row = &log.rows[0];
        assert_eq!(row.suite, "judge");
        let judge = row.judge.as_ref().expect("judge row");
        assert_eq!(judge.decided_by, DecidedBy::SwapDisagreement);
        assert_eq!((judge.gold_first, judge.prediction_first), (Some(true), Some(false)));
        let text = std::fs::read_to_string(eval.join("r-judge/results.jsonl")).expect("read");
        assert!(text.contains("\"decided_by\":\"swap_disagreement\""), "{text}");
        assert!(log.is_done(&TaskKey::buffered("judge", "function_body-abc123-L10")));
        let pre_c: TaskRow = serde_json::from_str(
            &text.replace(",\"judge\":{", ",\"grade\":null,\"judge_dropped\":{").replacen("\"judge_dropped\":{", "\"grade\":null,\"unused\":{", 0),
        )
        .map_or_else(|_| serde_json::from_str::<TaskRow>(&text.split(",\"judge\"").next().map(|s| format!("{s}}}")).expect("prefix")).expect("a row without the field parses"), |r| r);
        assert!(pre_c.judge.is_none());
    }
```

(The last assertion's intent: a row written before slice C — no `judge` key — loads with `judge: None`. If the string surgery reads poorly, replace it with a literal pre-C row: copy one line from `eval/20260830T072140Z-qwen3.8-flash-next/results.jsonl` into the test as a `const` and assert `judge.is_none()`.)

- [ ] **Step 2: Run** — `cargo test --lib bench::store::tests::a_judge_row_round_trips` → compile error.

- [ ] **Step 3: Implement** — in `store.rs` after `ExecRow`:

```rust
/// The suite a verdict row is recorded under — its own suite, because the
/// codebase row it judges was flushed long before the judge phase ran.
pub const JUDGE_SUITE: &str = "judge";

/// How a judge row was settled (spec C §5). "Not at all" is one of the ways.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecidedBy {
    SwapAgreement,
    SwapDisagreement,
    Identical,
    Skipped,
}

/// One crossing's verdict: the two raw answers, what they settle to, and why
/// there is none when there is none.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JudgeRow {
    /// `None` = the two orders disagreed, or the crossing was skipped.
    pub equivalent: Option<bool>,
    /// The raw answer with the gold shown as A; `None` when that order was skipped.
    pub gold_first: Option<bool>,
    /// The raw answer with the prediction shown as A.
    pub prediction_first: Option<bool>,
    pub decided_by: DecidedBy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
    pub judge_secs: f64,
}
```

`TaskRow` gains, after `codebase`:

```rust
    /// Present on `judge` rows only: the verdict for the codebase row with
    /// the same `task_id`. Rows written before slice C load as `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge: Option<JudgeRow>,
```

`Task` gains `pub judge: Option<JudgeRow>`; `append` copies it (`judge: task.judge`). Add:

```rust
/// Every verdict row, in file order.
pub(crate) fn judge_rows(log: &RunLog) -> Vec<&TaskRow> {
    rows_of(log, JUDGE_SUITE).filter(|r| r.judge.is_some()).collect()
}
```

`cargo build`; add `judge: None,` to every literal the compiler names.

- [ ] **Step 4: Run** — `cargo test --lib bench && make lint` → PASS.

- [ ] **Step 5: Commit** — `git commit -m "feat(store): JudgeRow — verdicts as their own append-only suite"`

---

### Task 6: `JudgeStamp` in the stamp; `compare` keeps comparing across judges

**Files:**
- Modify: `src/core/bench/stamp.rs:13-57` (`Stamp`), `:71-107` (`first_mismatch`), tests `:147-`; `src/core/bench/compare.rs:179-186` (`assert_same_environment`)

**Interfaces:**
- Produces: `pub struct JudgeStamp { model, quant, revision, arch, rubric_hash: String, max_tokens: u32, reasoning_effort: String, min_consistency_pct: u32 }` (`Clone, PartialEq, Eq, Serialize, Deserialize, deny_unknown_fields`); `Stamp.judge: Option<JudgeStamp>` (`#[serde(default, skip_serializing_if = "Option::is_none")]`); `first_mismatch` names `"judge"` last.

- [ ] **Step 1: Write the failing tests** (stamp tests; `stamp()` helper exists)

```rust
    fn judged() -> super::JudgeStamp {
        super::JudgeStamp {
            model: "gpt-oss-20b".into(),
            quant: "F16".into(),
            revision: "d449b42d93e1".into(),
            arch: "gpt-oss".into(),
            rubric_hash: "9f8e7d6c5b4a".into(),
            max_tokens: 512,
            reasoning_effort: "low".into(),
            min_consistency_pct: 70,
        }
    }

    #[test]
    fn a_differing_judge_is_the_last_field_named_and_an_absent_one_round_trips() {
        let mut with = stamp();
        with.judge = Some(judged());
        let mut other = with.clone();
        other.judge.as_mut().map(|j| j.rubric_hash = "000000000000".into());
        assert_eq!(first_mismatch(&with, &other), Some("judge"));
        assert_eq!(first_mismatch(&with, &stamp()), Some("judge"), "absent vs present differs");
        let json = serde_json::to_string(&stamp()).expect("json");
        assert!(!json.contains("\"judge\""), "no judge, no key: {json}");
        let back: Stamp = serde_json::from_str(&json).expect("parse");
        assert_eq!(back.judge, None);
        let mut ctx_differs = with.clone();
        ctx_differs.ctx = 1;
        assert_eq!(first_mismatch(&with, &ctx_differs), Some("ctx"), "earlier fields still win");
    }
```

And in `compare.rs` tests (find the existing `assert_same_environment` test near the `RunPair` helpers; add):

```rust
    #[test]
    fn a_differing_judge_does_not_refuse_the_comparison() {
        let a = run_with_stamp(stamp_a());
        let mut b = run_with_stamp(stamp_a());
        b.head.stamp.judge = Some(crate::core::bench::stamp::JudgeStamp {
            model: "gpt-oss-20b".into(), quant: "F16".into(), revision: "d449b42d93e1".into(),
            arch: "gpt-oss".into(), rubric_hash: "9f8e7d6c5b4a".into(), max_tokens: 512,
            reasoning_effort: "low".into(), min_consistency_pct: 70,
        });
        assert!(super::assert_same_environment(&a, &b).is_ok());
    }
```

(Use whatever the existing compare tests call their run/stamp builders — read `compare.rs` tests for the helper names and reuse them; do not add a second builder.)

- [ ] **Step 2: Run** — `cargo test --lib bench::stamp bench::compare` → compile error.

- [ ] **Step 3: Implement** — `stamp.rs`:

```rust
/// The judge a run's `equiv` column was measured with (spec C §5) — the
/// instrument, its budget and its floor, so a report is read against what
/// voided or kept the column, not against today's config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JudgeStamp {
    pub model: String,
    pub quant: String,
    /// The pinned HF revision, first twelve characters.
    pub revision: String,
    pub arch: String,
    pub rubric_hash: String,
    pub max_tokens: u32,
    pub reasoning_effort: String,
    pub min_consistency_pct: u32,
}
```

`Stamp` gains `#[serde(default, skip_serializing_if = "Option::is_none")] pub judge: Option<JudgeStamp>,` as the last field. `first_mismatch`'s array becomes `[(&'static str, bool); 21]` with `("judge", a.judge != b.judge)` appended. Update the module doc ("The 20-field…" → "The 21-field…"). In `compare.rs::assert_same_environment` add `b_env.judge.clone_from(&a.head.stamp.judge);` — the judge is a scoring instrument, compared by §8's own line, not an environment field.

- [ ] **Step 4: Run** — `cargo test --lib bench && make lint` → PASS.

- [ ] **Step 5: Commit** — `git commit -m "feat(stamp): JudgeStamp — the instrument, budget and floor a run was judged under"`

---

### Task 7: The report — `equiv` cell, header clause, trailer

**Files:**
- Modify: `src/core/bench/store.rs:757-830` (`render_codebase`, `Header`, `codebase_header`), `:917-935` (`scores_line`), `:1152-1180` (beside `exec_trailer`); tests at the bottom

**Interfaces:**
- Consumes: `judge::consistency_pct`, `judge_rows`, `JudgeStamp`.
- Produces: `scores_line(label, group, judge: Option<&str>)`; `fn judge_cell(kept: &[&TaskRow], log: &RunLog) -> Option<String>`; `fn judge_clause(log: &RunLog) -> String`; `fn judge_trailer(kept: &[&TaskRow], log: &RunLog) -> String`. Exact strings below.

- [ ] **Step 1: Write the failing tests** (store tests). Build a run with `head()` whose stamp has `judge: Some(JudgeStamp{ model: "gpt-oss-20b", quant: "F16", revision: "1a2b3c4d5e6f", arch: "gpt-oss", rubric_hash: "9f8e7d6c5b4a", max_tokens: 512, reasoning_effort: "low", min_consistency_pct: 70 })`, six `function_body` codebase rows (`task_id` `fb-1`…`fb-6`, use the existing `codebase_task()`-style helper in the tests — read the B2 tests for the helper that builds a codebase `Task`) and judge rows: `fb-1` `Identical` (`equivalent: Some(true)`), `fb-2`..`fb-4` `SwapAgreement` with `Some(true), Some(false), Some(false)` (`gold_first`/`prediction_first` equal), `fb-5` `SwapDisagreement` (`Some(true)/Some(false)`), `fb-6` `Skipped` with `skipped: Some("reply was not the schema: yes")`, each `judge_secs: 2.1`.

```rust
    #[test]
    fn the_function_body_line_carries_the_equiv_cell_and_the_header_names_the_judge() {
        let out = render_codebase(&judged_run());
        assert!(out.contains("; judge: gpt-oss-20b (F16@1a2b3c4d5e6f, gpt-oss, effort low) rubric 9f8e7d6c5b4a, swap consistency 75% (3 of 4)"), "{out}");
        assert!(out.contains("equiv 0.50 (n=4 judged of 6; 1 undecided)"), "identical counts as true, 2 true of 4: {out}");
        assert!(out.contains("             judge: 1 identical, 4 called, 1 undecided, 1 skipped; 2.1 s median per verdict\n"), "{out}");
        assert!(out.contains("             warning: 1 reply was not the schema — the grammar was not enforced for this judge and the column is measuring the prompt alone\n"), "{out}");
        let in_file_line = out.lines().find(|l| l.contains("in_file")).unwrap_or_default();
        assert!(!in_file_line.contains("equiv"), "other tiers print no equiv cell: {in_file_line}");
    }

    #[test]
    fn below_the_floor_the_column_is_voided_with_both_numbers() {
        let mut log = judged_run();
        log.head.stamp.judge.as_mut().map(|j| j.min_consistency_pct = 80);
        let out = render_codebase(&log);
        assert!(out.contains("equiv voided (swap consistency 75% < 80%)"), "{out}");
    }

    #[test]
    fn without_a_judge_the_trailer_says_so_and_an_unjudged_run_says_how_to_resume() {
        let out = render_codebase(&codebase_only_run());
        assert!(out.contains("             judge skipped: --judge not given\n"), "{out}");
        let mut stamped = judged_run();
        stamped.rows.retain(|r| r.suite != "judge");
        let out = render_codebase(&stamped);
        assert!(out.contains("             judge: 0 of 6 crossings judged — resume with --judge gpt-oss-20b\n"), "{out}");
    }
```

- [ ] **Step 2: Run** — fails (`judged_run` undefined / strings absent).

- [ ] **Step 3: Implement**

`render_codebase`: build `let judge = judge_cell(&kept, log);` and pass `judge.as_deref()` to the `function_body` `scores_line`, `None` to the others (including the two calls inside `cross_lines` — give `cross_lines` the same third argument `None`); append `judge_trailer(&kept, log)` after `exec_trailer`. `codebase_header` appends `judge_clause(log)` after `exec_clause(header.stamp)` — give `Header` a `log: &RunLog` field instead of `stamp` (the header reads both; three fields keep it a struct).

```rust
/// `equiv 0.60 (n=5 judged of 6; 1 undecided)`, or the void, or `None`
/// when the run has no judge rows at all.
fn judge_cell(kept: &[&TaskRow], log: &RunLog) -> Option<String> {
    let stamp = log.head.stamp.judge.as_ref()?;
    let rows: Vec<&JudgeRow> = judge_rows(log).iter().filter_map(|r| r.judge.as_ref()).collect();
    if rows.is_empty() {
        return None;
    }
    let total = tier_tasks(kept, TaskTier::FunctionBody);
    if let Some(rate) = crate::core::bench::judge::consistency_pct(&rows)
        && rate < stamp.min_consistency_pct
    {
        return Some(format!("equiv voided (swap consistency {rate}% < {}%)", stamp.min_consistency_pct));
    }
    let judged: Vec<bool> = rows.iter().filter_map(|r| r.equivalent).collect();
    let undecided = rows.iter().filter(|r| r.decided_by == DecidedBy::SwapDisagreement).count();
    if judged.is_empty() {
        return Some(format!("equiv n/a (0 judged of {total}; {undecided} undecided)"));
    }
    let mean = judged.iter().filter(|&&e| e).count() as f64 / as_f64(judged.len());
    Some(format!("equiv {mean:.2} (n={} judged of {total}; {undecided} undecided)", judged.len()))
}

/// `; judge: <model> (<quant>@<rev12>, <arch>, effort <e>) rubric <hash>, swap consistency N% (k of n)`.
fn judge_clause(log: &RunLog) -> String {
    let Some(j) = log.head.stamp.judge.as_ref() else {
        return String::new();
    };
    let rows: Vec<&JudgeRow> = judge_rows(log).iter().filter_map(|r| r.judge.as_ref()).collect();
    let answered = rows.iter().filter(|r| r.gold_first.is_some() && r.prediction_first.is_some()).count();
    let agreed = rows.iter().filter(|r| r.gold_first.is_some() && r.gold_first == r.prediction_first).count();
    let consistency = crate::core::bench::judge::consistency_pct(&rows)
        .map_or_else(|| "n/a".to_owned(), |rate| format!("{rate}% ({agreed} of {answered})"));
    format!(
        "; judge: {} ({}@{}, {}, effort {}) rubric {}, swap consistency {consistency}",
        j.model, j.quant, j.revision, j.arch, j.reasoning_effort, j.rubric_hash
    )
}

fn judge_trailer(kept: &[&TaskRow], log: &RunLog) -> String {
    let Some(stamp) = log.head.stamp.judge.as_ref() else {
        return "             judge skipped: --judge not given\n".to_owned();
    };
    let rows: Vec<&JudgeRow> = judge_rows(log).iter().filter_map(|r| r.judge.as_ref()).collect();
    let total = tier_tasks(kept, TaskTier::FunctionBody);
    if rows.is_empty() {
        return format!("             judge: 0 of {total} crossings judged — resume with --judge {}\n", stamp.model);
    }
    let count = |d: DecidedBy| rows.iter().filter(|r| r.decided_by == d).count();
    let called = rows.iter().filter(|r| r.gold_first.is_some()).count();
    let secs: Vec<f64> = rows.iter().filter(|r| r.gold_first.is_some()).map(|r| r.judge_secs).collect();
    let mut out = format!(
        "             judge: {} identical, {called} called, {} undecided, {} skipped; {:.1} s median per verdict\n",
        count(DecidedBy::Identical),
        count(DecidedBy::SwapDisagreement),
        count(DecidedBy::Skipped),
        median(&secs).unwrap_or(0.0)
    );
    let not_schema = rows.iter().filter(|r| r.skipped.as_deref().is_some_and(|s| s.starts_with("reply was not the schema"))).count();
    if not_schema > 0 {
        out.push_str(&format!("             warning: {not_schema} reply was not the schema — the grammar was not enforced for this judge and the column is measuring the prompt alone\n"));
    }
    out
}
```

`judge_trailer` is 27 lines; `judge_cell` 22. Skipped rows' `judge_secs` are excluded from the median by the `gold_first.is_some()` filter (a skip that never called has no time); `called` counts crossings with at least the first order answered. (`tier_tasks` exists at `store.rs:880`.)

- [ ] **Step 4: Run** — `cargo test --lib bench::store && make lint` → PASS.

- [ ] **Step 5: Commit** — `git commit -m "feat(store): the equiv cell, the judge header clause and trailer, voided below the floor"`

---

### Task 8: `compare` — the `equiv` row, or why there is none

**Files:**
- Modify: `src/core/bench/compare.rs:35` (`CodebaseComparison`), `:404-426` (`compare_codebase`), `:700-720` (`render_codebase`); tests

**Interfaces:**
- Produces: `CodebaseComparison.judge_note: Option<String>`; an `equiv` `TierDelta` (group `function_body`, tier `equiv`) pushed onto `tiers` when both runs' `stamp.judge` are `Some` and equal.

- [ ] **Step 1: Write the failing tests** (compare tests; reuse the existing run builders; add judge rows through the same `Task`/`TaskRow` builders as Task 7)

```rust
    #[test]
    fn a_shared_judge_yields_an_equiv_row_with_the_paired_sign_test() {
        // a: fb-1 true, fb-2 false, fb-3 true ; b: fb-1 false, fb-2 false, fb-3 undecided
        let (a, b) = judged_pair();
        let cmp = super::compare_runs(&a, &b, 5.0).expect("compare");
        let equiv = cmp.codebase.tiers.iter().find(|t| t.tier == "equiv").expect("equiv row");
        assert_eq!(equiv.group, "function_body");
        assert_eq!((equiv.a_better, equiv.b_better, equiv.ties), (1, 0, 1), "fb-3 is not paired: b abstained");
        assert!(cmp.codebase.judge_note.is_none());
        let out = super::render_comparison(&RunPair { a: &a, b: &b }, &cmp);
        assert!(out.contains("function_body equiv"), "{out}");
    }

    #[test]
    fn a_differing_or_absent_judge_prints_a_note_and_leaves_the_tiers_alone() {
        let (a, mut b) = judged_pair();
        b.head.stamp.judge = None;
        let cmp = super::compare_runs(&a, &b, 5.0).expect("compare");
        assert!(cmp.codebase.tiers.iter().all(|t| t.tier != "equiv"));
        assert_eq!(cmp.codebase.judge_note.as_deref(), Some("equiv: not compared (judge differs: a=gpt-oss-20b@1a2b3c4d5e6f/9f8e7d6c5b4a b=none)"));
        let out = super::render_comparison(&RunPair { a: &a, b: &b }, &cmp);
        assert!(out.contains("  equiv: not compared (judge differs: a=gpt-oss-20b@1a2b3c4d5e6f/9f8e7d6c5b4a b=none)\n"), "{out}");
    }
```

(Check `compare_runs`'s real signature at `compare.rs:128` and call it as it is.)

- [ ] **Step 2: Run** — fails.

- [ ] **Step 3: Implement**

```rust
/// One judged verdict per task id, as `1.0`/`0.0` — abstentions and skips are
/// absent, so a pair exists only where both runs reached a verdict.
fn judge_values(log: &RunLog) -> std::collections::BTreeMap<&str, f64> {
    store::judge_rows(log)
        .into_iter()
        .filter_map(|r| Some((r.task_id.as_str(), r.judge.as_ref()?.equivalent?)))
        .map(|(id, e)| (id, if e { 1.0 } else { 0.0 }))
        .collect()
}

fn judge_label(stamp: &Stamp) -> String {
    stamp.judge.as_ref().map_or_else(
        || "none".to_owned(),
        |j| format!("{}@{}/{}", j.model, j.revision, j.rubric_hash),
    )
}

/// The `equiv` row when both runs were judged by the same instrument, or
/// the note saying why they were not compared.
fn compare_judge(pair: &RunPair) -> (Option<TierDelta>, Option<String>) {
    let (ja, jb) = (&pair.a.head.stamp.judge, &pair.b.head.stamp.judge);
    match (ja, jb) {
        (None, None) => (None, None),
        (Some(a), Some(b)) if a == b => {
            let (va, vb) = (judge_values(pair.a), judge_values(pair.b));
            let values: Vec<(f64, f64)> = va.iter().filter_map(|(id, a)| Some((*a, *vb.get(id)?))).collect();
            if values.is_empty() {
                return (None, None);
            }
            let wins = win_counts(&values);
            (Some(TierDelta {
                group: TaskTier::FunctionBody.label().to_owned(),
                tier: "equiv".to_owned(),
                mean_a: mean(values.iter().map(|v| v.0)),
                mean_b: mean(values.iter().map(|v| v.1)),
                a_better: wins.a,
                b_better: wins.b,
                ties: wins.ties,
                verdict: sign_test(wins.a, wins.b),
            }), None)
        }
        _ => (None, Some(format!(
            "equiv: not compared (judge differs: a={} b={})",
            judge_label(&pair.a.head.stamp),
            judge_label(&pair.b.head.stamp)
        ))),
    }
}
```

`compare_codebase` calls `let (equiv, judge_note) = compare_judge(pair);`, pushes `equiv` onto `tiers` after the loop, and sets `judge_note` on `CodebaseComparison` (add the field, `pub judge_note: Option<String>`). `render_codebase` appends `format!("  {note}\n")` after the tier lines when `judge_note` is `Some`. Column width for the tier column already derives from the longest label (`column_width`), so `equiv` needs nothing.

- [ ] **Step 4: Run** — `cargo test --lib bench::compare && make lint` → PASS.

- [ ] **Step 5: Commit** — `git commit -m "feat(compare): the equiv row under a shared judge, and the note when the judges differ"`

---

### Task 9: The three `Judge*` errors

**Files:**
- Modify: `src/error.rs` (after `ExecWorktreeDirty` at `:362`), tests in the same file

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn the_judge_refusals_name_the_remedy() {
        let no_role = ChekovError::JudgeNoRole { name: "gemma-3-12b-it".into() }.to_string();
        assert!(no_role.contains("add `role = \"judge\"` to its entry"), "{no_role}");
        let family = ChekovError::JudgeFamilyConflict {
            judge: "qwen3.8-27b".into(), candidate: "ornith-1.5-35b-a3b".into(), family: "qwen35".into(),
        }
        .to_string();
        assert!(family.contains("qwen35") && family.contains("ornith-1.5-35b-a3b"), "{family}");
        let server = ChekovError::JudgeNeedsTheServer.to_string();
        assert!(server.contains("bench never stops a server it did not start; stop it or drop --judge"), "{server}");
    }
```

- [ ] **Step 2: Run** — compile error.

- [ ] **Step 3: Implement**

```rust
    #[error(
        "'{name}' is registered without `role = \"judge\"` — a judge is named on purpose; \
         add `role = \"judge\"` to its entry in models.toml (a different family from every \
         candidate), then retry"
    )]
    JudgeNoRole { name: String },

    #[error(
        "judge '{judge}' and candidate '{candidate}' are the same family ({family}) — a model \
         judging its sibling produces a rigged table; name a judge whose GGUF \
         `general.architecture` is not {family}"
    )]
    JudgeFamilyConflict { judge: String, candidate: String, family: String },

    #[error(
        "--judge needs to load the judge after the candidates are down, but the running \
         server is one bench did not start — bench never stops a server it did not start; \
         stop it or drop --judge"
    )]
    JudgeNeedsTheServer,
```

- [ ] **Step 4: Run** — `cargo test --lib error && make lint` → PASS.

- [ ] **Step 5: Commit** — `git commit -m "feat(error): JudgeNoRole, JudgeFamilyConflict, JudgeNeedsTheServer"`

---

### Task 10: The judge step in the plan and the estimate

**Files:**
- Modify: `src/core/bench/lifecycle.rs:118-140` (`StepAction`), `:144-168` (`estimate_secs`), `:170-174` (`needs_confirm`), `:186-196` (`step_line`); tests

**Interfaces:**
- Produces: `StepAction::Judge` — rendered `judge  launch + teardown  weights X GiB`; counts as a launch for `needs_confirm`; `estimate_secs` adds its load time and NO sweep time. `pub const JUDGE_SECS_PER_VERDICT: u64 = 2;` and `pub fn judge_estimate_secs(crossings: u64) -> u64` (= `crossings * 2 orders * JUDGE_SECS_PER_VERDICT`).

- [ ] **Step 1: Write the failing tests** (lifecycle tests; a `plan()`/`step()` helper likely exists — reuse)

```rust
    #[test]
    fn a_judge_step_confirms_loads_and_never_sweeps() {
        let steps = [
            BenchStep { model: "ornith-1.5-35b-a3b".into(), action: StepAction::Launch, weights_bytes: Some(GIB) },
            BenchStep { model: "gpt-oss-20b".into(), action: StepAction::Judge, weights_bytes: Some(GIB) },
        ];
        let plan = crate::core::bench::sweep::SweepPlan { depths: vec![1024], repetitions: 1, max_tokens: 60 };
        let one = estimate_secs(&steps[..1], &plan);
        let both = estimate_secs(&steps, &plan);
        assert_eq!(both - one, 4, "a judge step costs its load (4 s/GiB) and no sweep");
        assert!(needs_confirm(&steps[1..]));
        assert!(render_plan(&steps, both).contains("  gpt-oss-20b  judge: launch + teardown  weights 1.0 GiB\n"));
        assert_eq!(judge_estimate_secs(6), 24);
    }
```

- [ ] **Step 2: Run** — compile error.

- [ ] **Step 3: Implement** — add the variant with a doc comment (`/// The judge, loaded once after every candidate is down (spec C §3).`); in `estimate_secs` the per-step closure becomes `match step.action { StepAction::Launch => load + sweep_ms, StepAction::Judge => load, StepAction::UseRunning => sweep_ms }` (write it as a small `step_ms(step, sweep_ms)` helper); `needs_confirm` matches `Launch | Judge`; `step_line` prints `"judge: launch + teardown"` for `Judge`. Add:

```rust
/// Two seconds a verdict on the 2026-08-30 probe (1.06 s gpt-oss-20b,
/// 0.78 s Gemma), rounded up; two orders per crossing.
pub const JUDGE_SECS_PER_VERDICT: u64 = 2;

#[must_use]
pub const fn judge_estimate_secs(crossings: u64) -> u64 {
    crossings * 2 * JUDGE_SECS_PER_VERDICT
}
```

- [ ] **Step 4: Run** — `cargo test --lib bench::lifecycle && make lint` → PASS.

- [ ] **Step 5: Commit** — `git commit -m "feat(lifecycle): the judge step — confirmed, loaded, never swept"`

---

### Task 11: `--judge` — resolution, stamp, plan, and the judge phase

**Files:**
- Modify: `src/commands/capability.rs:99-138` (`BenchOpts`), `:796-835` (`BenchArgs`, `From`), `:865-870` (`RunInputs`), `:919-1030` (`bench_steps`, `bench_estimate`, `render_dry_run`, `bench`, `run_candidates`), `:1449-1470` (`open_run`), `:1582-1745` (`HeadInputs`, `assemble_stamp`, `build_head`); new `pub struct JudgePlan` + `resolve` in `src/core/bench/judge.rs` (the plan is data the core owns; the command layer builds it)
- Test: `src/commands/capability.rs` tests (unit, canned registry) and `src/core/bench/judge.rs` tests

**Interfaces:**
- Consumes: Tasks 1–10.
- Produces: `judge::JudgePlan { judge: Effective, arch: String, rubric_hash: String, max_tokens: u32, min_consistency_pct: u32, reasoning_effort: ReasoningEffort }` with `fn stamp(&self) -> JudgeStamp` and `fn forced(&self, schema: &Value) -> Forced`; `judge::family_conflict(judge: (&str, &str), candidates: &[(String, String)]) -> Option<ChekovError>` (pure: names + archs in, the refusal out); command-layer `resolve_judge(ctx, args, candidates) -> Result<Option<JudgePlan>, ChekovError>`, `run_judge_phase(ctx, runs: &[PathBuf], plan: &JudgePlan) -> Result<(), ChekovError>`, `judge_run(wire, dir, plan) -> Result<usize, ChekovError>`.

- [ ] **Step 1: Write the failing tests**

In `judge.rs` tests:

```rust
    #[test]
    fn a_family_conflict_names_judge_candidate_and_family() {
        let candidates = vec![("ornith-1.5-35b-a3b".to_owned(), "qwen35moe".to_owned()), ("minimax-m2.7".to_owned(), "minimax-m2".to_owned())];
        let err = super::family_conflict(("qwen3.8-27b", "qwen35"), &candidates).expect("conflict");
        assert!(matches!(err, crate::error::ChekovError::JudgeFamilyConflict { ref candidate, ref family, .. } if candidate == "ornith-1.5-35b-a3b" && family == "qwen35"));
        assert!(super::family_conflict(("gpt-oss-20b", "gpt-oss"), &candidates).is_none());
        let itself = vec![("gpt-oss-20b".to_owned(), "gpt-oss".to_owned())];
        assert!(super::family_conflict(("gpt-oss-20b", "gpt-oss"), &itself).is_some(), "a judge among the candidates conflicts with itself");
    }

    #[test]
    fn the_plan_stamps_what_it_was_built_from() {
        let plan = super::JudgePlan {
            judge: crate::core::registry::Effective {
                name: "gpt-oss-20b".into(), ctx_size: 98_304, flags: vec![],
                entry: crate::core::registry::ModelEntry {
                    repo: "unsloth/gpt-oss-20b-GGUF".into(), quant: "F16".into(),
                    revision: "d449b42d93e1c2c7bda5312f5c25c8fb91dfa9b4".into(),
                    path: "models/gpt-oss-20b@d449b42d93e1".into(), first_shard: "gpt-oss-20b-F16.gguf".into(),
                    hermes_ok: true, ctx_size: None, extra_flags: vec![], role: Some(crate::core::registry::ModelRole::Judge),
                },
            },
            arch: "gpt-oss".into(),
            rubric_hash: super::rubric_hash(),
            max_tokens: 512,
            min_consistency_pct: 70,
            reasoning_effort: crate::core::config::ReasoningEffort::Low,
        };
        let stamp = plan.stamp();
        assert_eq!((stamp.model.as_str(), stamp.revision.as_str(), stamp.reasoning_effort.as_str(), stamp.max_tokens), ("gpt-oss-20b", "d449b42d93e1", "low", 512));
    }
```

In `capability.rs` tests (there is a canned-`Ctx` pattern for `bench` unit tests — read the existing `server_use_rule`/`resolve_candidates` tests and reuse their registry fixture):

```rust
    #[test]
    fn judge_without_a_role_and_a_reused_server_are_refused_before_any_launch() {
        // registry: candidate "ornith" (qwen35moe) + "plain" without role
        let err = judge_refusal(Some("plain"), &[("ornith", StepAction::Launch)]);
        assert!(matches!(err, Some(ChekovError::JudgeNoRole { .. })));
        let err = judge_refusal(Some("gpt-oss-20b"), &[("ornith", StepAction::UseRunning)]);
        assert!(matches!(err, Some(ChekovError::JudgeNeedsTheServer)));
        assert!(judge_refusal(None, &[("ornith", StepAction::Launch)]).is_none());
    }
```

(`judge_refusal` is a test helper around the pure pieces of `resolve_judge` — `role_check` and `server_check` below — with no GGUF on disk; the family check needs a real header and is covered by `family_conflict`'s unit test plus the live run.)

- [ ] **Step 2: Run** — compile errors.

- [ ] **Step 3: Implement** — in `judge.rs`:

```rust
/// Everything the judge phase needs, resolved before any launch (spec C §3).
pub struct JudgePlan {
    pub judge: crate::core::registry::Effective,
    pub arch: String,
    pub rubric_hash: String,
    pub max_tokens: u32,
    pub min_consistency_pct: u32,
    pub reasoning_effort: crate::core::config::ReasoningEffort,
}

impl JudgePlan {
    #[must_use]
    pub fn stamp(&self) -> crate::core::bench::stamp::JudgeStamp {
        let entry = &self.judge.entry;
        crate::core::bench::stamp::JudgeStamp {
            model: self.judge.name.clone(),
            quant: entry.quant.clone(),
            revision: entry.revision[..12.min(entry.revision.len())].to_owned(),
            arch: self.arch.clone(),
            rubric_hash: self.rubric_hash.clone(),
            max_tokens: self.max_tokens,
            reasoning_effort: self.reasoning_effort.as_str().to_owned(),
            min_consistency_pct: self.min_consistency_pct,
        }
    }

    /// The forced wire's inputs for a judge request.
    #[must_use]
    pub fn forced<'a>(&self, schema: &'a Value) -> crate::core::bench::runner::Forced<'a> {
        crate::core::bench::runner::Forced { schema, reasoning_effort: Some(self.reasoning_effort.as_str()) }
    }
}

/// The refusal when the judge shares a family with any candidate — or IS one.
#[must_use]
pub fn family_conflict(judge: (&str, &str), candidates: &[(String, String)]) -> Option<crate::error::ChekovError> {
    let (judge_name, judge_arch) = judge;
    candidates
        .iter()
        .find(|(name, arch)| name == judge_name || family_key(arch) == family_key(judge_arch))
        .map(|(name, _)| crate::error::ChekovError::JudgeFamilyConflict {
            judge: judge_name.to_owned(),
            candidate: name.clone(),
            family: family_key(judge_arch).to_owned(),
        })
}
```

(`Forced.reasoning_effort` is `Option<&'a str>` tied to the schema lifetime; `as_str()` returns `&'static str`, which coerces.)

In `capability.rs`:

`BenchOpts`:

```rust
    /// A registered `role = "judge"` model, loaded in its own phase after
    /// every candidate is down, answering one position-swapped binary
    /// question per `function_body` crossing (spec C). Refused before any
    /// launch when it lacks the role, shares a family with a candidate, or
    /// would have to stop a server bench did not start.
    #[arg(long, requires = "codebase")]
    pub judge: Option<String>,
```

`BenchArgs` gains `judge: Option<&'a str>` (mirror in `From`). `RunInputs` gains `judge: Option<&'a JudgePlan>`.

```rust
/// `--judge`, resolved before any launch: role, no foreign server to stop,
/// no shared family — each a refusal that names its remedy.
fn resolve_judge(
    ctx: &Ctx,
    args: &BenchArgs,
    candidates: &[(crate::core::registry::Effective, crate::core::bench::lifecycle::StepAction)],
) -> Result<Option<crate::core::bench::judge::JudgePlan>, ChekovError> {
    use crate::core::bench::{judge, lifecycle::StepAction};
    let Some(name) = args.judge else {
        return Ok(None);
    };
    let reg = ctx.registry()?;
    let eff = reg.effective(name)?;
    if eff.entry.role != Some(crate::core::registry::ModelRole::Judge) {
        return Err(ChekovError::JudgeNoRole { name: name.to_owned() });
    }
    if candidates.iter().any(|(_, action)| *action == StepAction::UseRunning) {
        return Err(ChekovError::JudgeNeedsTheServer);
    }
    let arch = arch_of(ctx, &eff)?;
    let archs: Vec<(String, String)> = candidates
        .iter()
        .map(|(c, _)| Ok((c.name.clone(), arch_of(ctx, c)?)))
        .collect::<Result<_, ChekovError>>()?;
    if let Some(conflict) = judge::family_conflict((name, &arch), &archs) {
        return Err(conflict);
    }
    let bench_cfg = &ctx.config.file.bench;
    Ok(Some(judge::JudgePlan {
        judge: eff,
        arch,
        rubric_hash: judge::rubric_hash(),
        max_tokens: bench_cfg.judge_max_tokens,
        min_consistency_pct: bench_cfg.judge_min_consistency_pct,
        reasoning_effort: bench_cfg.judge_reasoning_effort,
    }))
}

/// `general.architecture` from a model's first shard — the family check
/// cannot proceed without it, so a missing shard is the existing preflight
/// refusal, not a guess.
fn arch_of(ctx: &Ctx, eff: &crate::core::registry::Effective) -> Result<String, ChekovError> {
    let path = crate::core::server::shard_path(&ctx.config, eff);
    Ok(crate::core::gguf::read_geometry(&path)?.arch)
}
```

(`resolve_judge` is 33 lines. `read_geometry`'s error already names the path.)

Stamp: `HeadInputs` gains `judge: Option<JudgeStamp>` (from `inputs.judge.map(JudgePlan::stamp)` in `head_inputs`); `assemble_stamp` sets `judge: inputs.judge.clone()`.

Plan and estimate: `bench_steps` takes `(ctx, candidates, judge: Option<&JudgePlan>)` and appends `BenchStep { model: plan.judge.name.clone(), action: StepAction::Judge, weights_bytes: weights_on_disk(ctx, &plan.judge.entry) }`; `bench_estimate` adds `judge_secs = inputs.judge.map_or(0, |_| lifecycle::judge_estimate_secs(u64::try_from(prepared.counts.function_body).unwrap_or(0) * candidates))` — pass the candidate count in `RunInputs` (`candidates: usize`); `render_dry_run` appends the line `+ judge: 2 orders × {crossings} verdicts, ~2 s each\n` after the codebase line when `inputs.judge` is `Some`.

`--resume` with a judge: in `open_run`, before `RunWriter::resume`, when `head.stamp.judge.is_some()`:

```rust
        store::adopt_judge(&eval.join(run_id), head.stamp.judge.as_ref())?;
```

with, in `store.rs`:

```rust
/// A run recorded before it had a judge takes the judge on resume: the head
/// is rewritten with the field added and nothing else changed. A run that
/// already names a different judge is left alone — `resume` refuses it.
pub fn adopt_judge(run_dir: &Path, judge: Option<&JudgeStamp>) -> Result<(), ChekovError> {
    let mut log = RunLog::load(run_dir)?;
    if log.head.stamp.judge.is_some() || judge.is_none() {
        return Ok(());
    }
    log.head.stamp.judge = judge.cloned();
    let stamp_path = run_dir.join("stamp.json");
    let json = serde_json::to_string_pretty(&log.head).map_err(|e| invalid(&stamp_path, e))?;
    std::fs::write(&stamp_path, json).map_err(|e| ChekovError::io(format!("writing {}", stamp_path.display()), e))
}
```

`bench()`: after `resolve_candidates`, `let judge = resolve_judge(ctx, args, &candidates)?;` goes into `inputs`; `run_candidates` returns `Vec<PathBuf>` (print each `run:` line as today); then `if let Some(plan) = judge.as_ref() { run_judge_phase(ctx, &dirs, plan)?; }` before `finish_codebase`. Keep `bench()` ≤ 40 lines by moving the confirm into `fn confirm_launches(steps, estimate, yes) -> Result<(), ChekovError>`.

The phase:

```rust
/// Launch the judge once, judge every run directory, tear it down (spec C §3).
fn run_judge_phase(ctx: &Ctx, runs: &[std::path::PathBuf], plan: &crate::core::bench::judge::JudgePlan) -> Result<(), ChekovError> {
    use crate::core::bench::runner;
    use crate::core::proxy::claude::ClaudeFacade;
    use crate::core::proxy::serve::Upstream;
    let pid = launch_candidate(ctx, &plan.judge)?;
    let upstream = Upstream { base_url: ctx.config.base_url(), api_key: ctx.config.file.server.api_key.clone() };
    ensure_ready(ctx, &upstream, &BenchSetup { eff: plan.judge.clone(), pid })?;
    let facade = ClaudeFacade::new(&plan.judge.name);
    let wire = runner::ProbeWire {
        http: ctx.http.as_ref(),
        facade: &facade,
        upstream: &upstream,
        pins: runner::SamplingPins { seed: ctx.config.file.bench.seed },
    };
    let outcome = runs.iter().try_for_each(|dir| {
        let verdicts = judge_run(&wire, dir, plan)?;
        eprintln!("chekov bench: judge '{}' — {verdicts} verdict(s) for {}", plan.judge.name, dir.display());
        print!("{}", crate::core::bench::store::render_codebase(&crate::core::bench::store::RunLog::load(dir)?));
        Ok::<(), ChekovError>(())
    });
    teardown_candidate(ctx, pid)?;
    outcome
}

/// Every eligible `function_body` crossing of one run, appended as it lands.
fn judge_run(wire: &crate::core::bench::runner::ProbeWire, dir: &std::path::Path, plan: &crate::core::bench::judge::JudgePlan) -> Result<usize, ChekovError> {
    use crate::core::bench::store::{self, RunLog, RunWriter, TaskKey};
    let log = RunLog::load(dir)?;
    let eval = dir.parent().ok_or_else(|| ChekovError::BenchRunInvalid { path: dir.to_path_buf(), reason: "no parent directory".into() })?;
    let run_id = dir.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let (mut writer, _) = RunWriter::resume(eval, run_id, &log.head)?;
    let mut count = 0;
    for row in log.rows.iter().filter(|r| r.suite == "codebase" && !store::is_unavailable(r)) {
        if log.is_done(&TaskKey::buffered(store::JUDGE_SUITE, &row.task_id)) {
            continue;
        }
        let Some(codebase) = row.codebase.as_ref() else { continue };
        let Some(judge_row) = verdict_for(wire, codebase, plan)? else { continue };
        writer.append(store::Task {
            suite: store::JUDGE_SUITE.into(),
            task_id: row.task_id.clone(),
            measure: crate::core::bench::codebase::run::empty_measure(),
            grade: None,
            transport: store::Transport::Buffered,
            codebase: None,
            judge: Some(judge_row),
        })?;
        count += 1;
    }
    Ok(count)
}

/// One crossing's judge row, or `None` when the row is not a judge row at all.
fn verdict_for(wire: &crate::core::bench::runner::ProbeWire, row: &crate::core::bench::store::CodebaseRow, plan: &crate::core::bench::judge::JudgePlan) -> Result<Option<crate::core::bench::store::JudgeRow>, ChekovError> {
    use crate::core::bench::judge::{self, Eligibility, Reply};
    use crate::core::bench::runner::cross_forced_with;
    use crate::core::bench::store::{DecidedBy, JudgeRow};
    let settled = |equivalent, decided_by, skipped, judge_secs| JudgeRow { equivalent, gold_first: None, prediction_first: None, decided_by, skipped, judge_secs };
    let pair = match judge::eligibility(row) {
        None => return Ok(None),
        Some(Eligibility::Identical) => return Ok(Some(settled(Some(true), DecidedBy::Identical, None, 0.0))),
        Some(Eligibility::Skipped(reason)) => return Ok(Some(settled(None, DecidedBy::Skipped, Some(reason.to_owned()), 0.0))),
        Some(Eligibility::Judge(pair)) => pair,
    };
    let schema = judge::schema();
    let started = std::time::Instant::now();
    let [first, second] = judge::requests(&pair, plan.max_tokens);
    let ask = |req| cross_forced_with(wire, req, &plan.forced(&schema)).map(|a| judge::parse_reply(&a.anthropic_body, plan.max_tokens));
    let (gold_first, prediction_first) = (ask(&first)?, ask(&second)?);
    let verdict = judge::combine(&gold_first, &prediction_first);
    let answer = |r: &Reply| match r { Reply::Answer(b) => Some(*b), Reply::Skipped(_) => None };
    Ok(Some(JudgeRow {
        equivalent: verdict.equivalent,
        gold_first: answer(&gold_first),
        prediction_first: answer(&prediction_first),
        decided_by: verdict.decided_by,
        skipped: verdict.skipped,
        judge_secs: started.elapsed().as_secs_f64() / 2.0,
    }))
}
```

A judge server that answers a non-2xx surfaces as `UpstreamRefused` from `cross_forced_with`; per spec §7 that crossing is `skipped("judge refused: <the server's words>")` and the phase continues — wrap `ask` so `Err(ChekovError::UpstreamRefused { .. })` becomes `Reply::Skipped(format!("judge refused: {e}"))` and every other error propagates (an `EndpointDown` stops the phase with rows so far intact). Keep `verdict_for` under 40 lines by moving `ask` into `fn ask_judge(wire, req, plan, schema) -> Result<Reply, ChekovError>`.

- [ ] **Step 4: Run** — `cargo test && make lint` → PASS. Then the dry run:

```
cargo build --release
./target/release/chekov capability bench --codebase . --judge gpt-oss-20b --dry-run
```

Expected: the codebase line, then `+ judge: 2 orders × 6 verdicts, ~2 s each`, then the plan with a final `  gpt-oss-20b  judge: launch + teardown  weights 12.8 GiB` line. (`gpt-oss-20b` still lacks `role = "judge"` at this point — expect `JudgeNoRole` first; add `role = "judge"` to the `[models.gpt-oss-20b]` and `[models.gemma-3-12b-it]` entries in `models.toml` by hand, rerun.)

- [ ] **Step 5: Commit** — `git commit -m "feat(capability): --judge — resolved before any launch, stamped, planned, and run as its own phase"`

---

### Task 12: Docs, changelog, ideas, spec pointers

**Files:**
- Modify: `README.md:102` (the `capability bench` row: add `--judge NAME` to the synopsis and one sentence: *`--judge` names a registered `role = "judge"` model of a different family from every candidate, loaded once after they are all down; it answers one position-swapped, grammar-forced binary question per `function_body` crossing, and the `equiv` column is voided below `[bench] judge_min_consistency_pct`. The 2026-08-30 probe recommends `gpt-oss-20b` (Apache-2.0); Gemma 3 12B also clears the gate.*), the `models.toml` section (document `role = "judge"`), the `[bench]` config keys table (three knobs with defaults and the one-line rationale each); `config.example.toml` (the three keys, commented, with defaults); `CHANGELOG.md` `[Unreleased] ### Added` (one entry, past-tense-free, from the spec's §0/§4/§5/§6 wording); `IDEAS.md:134` (replace `slice C (--judge) OPEN` with `slice C (--judge) SHIPPED 2026-08-30 (gpt-oss-20b recommended; probe in the spec §3.0)`); `docs/capability-spec.md:897` status line (add `and C (--judge, position-swapped binary judge as its own phase)`), and a one-line pointer under §10 to the slice C spec and its §9 departures.

- [ ] **Step 1: Edit each file as listed.** No code.
- [ ] **Step 2: `make lint && make test`** (the README is read by `tests/pull_dry_run.rs`? — no; but the doc tests in `lib.rs` might; run the suite anyway).
- [ ] **Step 3: Commit** — `git commit -m "docs: --judge in the README, the three [bench] knobs, changelog, ideas and spec pointers"`

---

### Task 13: Live run and the PR

- [ ] **Step 1:** `cargo build --release && ./target/release/chekov capability bench --codebase . --allow-exec --judge gpt-oss-20b --yes` with `ornith-1.5-35b-a3b` active (no server running). Expected on stderr: the candidate launch/teardown, `chekov bench: started 'gpt-oss-20b'`, `judge 'gpt-oss-20b' — N verdict(s)`, budget released; on stdout: the codebase block twice (before and after the judge phase), the second with the `equiv` cell, the `judge:` header clause and trailer. Paste both blocks into the PR body.
- [ ] **Step 2:** `./target/release/chekov capability compare <the new run> 20260830T072140Z-qwen3.8-flash-next` → the `equiv: not compared (judge differs: a=gpt-oss-20b@d449b42d93e1/<hash> b=none)` line and unchanged tier rows. Paste it.
- [ ] **Step 3:** `./target/release/chekov capability bench --codebase . --resume <the new run> --judge gpt-oss-20b --yes` → every codebase and judge row skipped, `0 verdict(s)`, no rows appended (`wc -l results.jsonl` unchanged). Paste the line count.
- [ ] **Step 4:** Open the PR against `develop` with `gh pr create --base develop` and a HEREDOC body: the spec link, the probe table from spec §3.0, the three pasted outputs, and the trailer `🤖 Generated with [Claude Code](https://claude.com/claude-code)` + the session URL. Do not merge; report the URL.

---

## Self-review

- **Spec coverage:** §2 role/refusals → Tasks 1, 9, 11; §2.1 family → 4, 11; §3.0 probe numbers → spec (done), README → 12; §3 lifecycle/`JudgePlan` → 10, 11; §4 eligibility, requests, schema, strict parse, truncation, rubric file, hash → 4; §5 `JudgeRow`, `DecidedBy::Skipped`, resume key, `JudgeStamp` with floor, `first_mismatch` → 5, 6; §6 report strings, warning line, compare row/note → 7, 8; §7 errors, `UpstreamRefused` → skip, `--judge` `requires`, `adopt_judge` on resume → 9, 11; §8 tests → each task; §11 files → all; `runner` `reasoning_effort` → 3; config knobs → 2.
- **Placeholders:** none; every step carries its code or its exact command.
- **Type consistency:** `Forced { schema: &Value, reasoning_effort: Option<&str> }` (3) is what `JudgePlan::forced` builds (11) and `cross_forced_with` takes (3, 11); `Reply`/`Verdict`/`Eligibility`/`Pair` (4) are what `verdict_for` consumes (11); `JudgeRow`/`DecidedBy`/`JUDGE_SUITE`/`judge_rows` (5) are what `judge.rs` (4), the report (7) and `compare` (8) read; `JudgeStamp` (6) is what `JudgePlan::stamp` (11) builds and `judge_cell`/`judge_clause`/`compare_judge` (7, 8) read; `ReasoningEffort::as_str` (2) feeds both the wire (11) and the stamp (6).
