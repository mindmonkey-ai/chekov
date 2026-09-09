# Bench Probe Suites v0 (slice-5 gap, part 3) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The corpus-free deterministic probe suites from spec §7.2 — `tool_emit` (BFCL-style AST match incl. abstention and missing-function cases), `grammar_gap` (the same cases with a JSON schema forced; the gap is the anti-self-deception device), and `instruction_adherence` (IFEval-style verifiable constraints, strict AND loose reported separately) — behind a `--suite` flag, with an authored v0 case set whose content is pinned by the prompt-set hash.

**Architecture:** `bench/probeset.rs` parses a compiled-in TOML (`bench/agentic_v0.toml`, `include_str!`) into typed cases; graders extend `bench/grade.rs` as pure functions over the ANTHROPIC artifact; `runner::cross_forced` injects `response_format: json_schema` into the forwarded body through the same wire-injection mechanism as the sampling pins. Rows land in the same JSONL store (suites `tool_emit`, `grammar_gap`, `instruction`), and `render_run` gains per-suite summary lines including both gaps.

**Spec:** `docs/capability-spec.md` §7.2 rows 1, 2, 6. **Deliberately deferred** (need §8/§9 corpora or open answers): `diff_fidelity`, `tool_loop`, `long_ctx_trace`, `hallucination` (slice 6), `think_leak` (§13 Q5 — is `--reasoning-format none` deliberate?).

**Deviations, flagged:**
- v0 ships a SEED set (10 tool cases: 7 call, 2 abstention, 1 missing-function; 12 instruction cases), not §7.2's 30/40 — the counts print with every summary (`no silent caps`), and the prompt-set hash pins the content so a grown set can never compare against a v0 run.
- `--suite` defaults to `throughput`, not the spec's `agentic`, until the agentic set reaches full strength — changing the default under a partial set would misrepresent what "bench" measures.
- `grammar_gap`'s forced pass covers only the `call` cases (a grammar that must emit a call cannot express abstention); the rendering states how many cases the forced pass covered.

## Global Constraints

As parts 1–2. Branch: `feat/capability-bench-probes` stacked on `feat/capability-bench-candidates` (PR #31).

---

### Task 1: the probe set (`bench/probeset.rs` + `bench/agentic_v0.toml`)

Schema (deny_unknown_fields):

```toml
version = 0

[[tool_emit]]
id = "te-001"
prompt = "…"
expect = "call"            # or "abstain"
golden_name = "read_file"  # call cases only
golden_args = '{"path":"src/main.rs"}'
[[tool_emit.tools]]
name = "read_file"
description = "…"
input_schema = '{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}'

[[instruction]]
id = "if-001"
prompt = "…"
checks = ["fenced_rust_only", "max_lines:12"]
```

```rust
pub struct ProbeSet { pub version: u32, pub tool_emit: Vec<ToolCase>, pub instruction: Vec<InstructionCase> }
pub struct ToolCase { pub id, pub prompt, pub expect: Expect, pub golden_name: Option<String>, pub golden_args: Option<String>, pub tools: Vec<ToolDef> }
pub enum Expect { Call, Abstain }
pub struct ToolDef { pub name, pub description, pub input_schema: String }   // JSON text, parsed on use
pub struct InstructionCase { pub id, pub prompt, pub checks: Vec<String> }

pub fn agentic_v0() -> Result<ProbeSet, ChekovError>       // include_str! + validation
pub fn content_hash() -> String                            // sha256 of the TOML text, 12 hex
```

Validation is loud: version must be 0; a `call` case must carry golden fields naming a tool in its own palette; ids unique. A test parses the real file and asserts the counts (7/2/1, 12) so a content edit that breaks shape fails the build.

Content: coding-agent-flavored (read/grep/edit/run-tests palettes). Abstention cases where the correct behavior is a plain answer; the missing-function case offers a palette that cannot do what was asked (must not fabricate a name).

Commit: `feat(bench): authored probe set v0 — tool cases and instruction constraints, content-hashed`

---

### Task 2: graders (`bench/grade.rs`)

```rust
/// BFCL-style AST match on the translated tool_use block: name + arguments
/// compared as parsed JSON (object key order never matters), never as text.
pub fn grade_tool_emit(anthropic_body: &str, case: &ToolCase) -> Grade
/// The forced pass: content text parsed as {"name","arguments"} JSON.
pub fn grade_forced(anthropic_body: &str, case: &ToolCase) -> Grade
/// (strict, loose): strict over the raw text, loose over the extracted
/// fenced region — the gap is a chattiness metric for an agent backend.
pub fn grade_instruction(anthropic_body: &str, case: &InstructionCase) -> (Grade, Grade)
```

Check vocabulary (table dispatch, §6 — one function per check): `fenced_rust_only`, `contains_fence`, `max_lines:N`, `no_unwrap`, `contains:S`, `not_contains:S`, `brace_balanced`, `single_function`. Unknown check name = loud validation error at probeset load, never a silent pass.

Grading rules pinned by tests: abstention fails on ANY tool_use (naming the fabricated tool); a call case fails on zero or multiple calls, a name mismatch, or differing arguments; argument comparison is `serde_json::Value` equality; a translation-broken body fails as translation, not as empty (the shipped rule).

Commit: `feat(bench): deterministic graders — AST tool match, forced-call parse, strict/loose constraints`

---

### Task 3: the forced wire (`runner::cross_forced`)

`cross` and `cross_forced` share one internal path; `cross_forced(wire, req, schema)` additionally injects `"response_format": {"type":"json_schema","json_schema":{"name":"tool_call","schema": <schema>}}` into the forwarded body. `pub fn probeset::forced_schema(case: &ToolCase) -> Value` builds `oneOf` arms `{name: const <tool>, arguments: <tool schema>}` over the case's palette.

Tests: the forwarded body carries both the pins AND the response_format; `forced_schema` yields one arm per tool with the palette's own schemas embedded.

Commit: `feat(bench): grammar-forced crossing — response_format injected on the wire`

---

### Task 4: suite selection + orchestration + rendering

- CLI: `--suite <throughput|agentic|all>` on `BenchOpts` (clap `ValueEnum`), default `throughput` (deviation above, stated in the help text).
- `run_suites` branches: agentic runs `tool_emit` (probe per case), `grammar_gap` (forced, `call` cases only, task ids `gg-<id>`), `instruction` (strict grade in `pass`, loose in the reason as `loose:pass|fail`). Rows append per case; `--resume` skips as everywhere.
- `prompt_set_hash` and `corpus_id` gain the agentic component when the suite includes it (`agentic-v0:<content_hash>`); a throughput-only run keeps today's values, so existing runs stay comparable.
- Estimate: `+8s` per agentic case when the suite includes it.
- `render_run` summary lines, computed from rows:
  - `tool_emit    7/10 (2 abstention, 1 missing-function among the cases)`
  - `grammar_gap  6/7 forced over the call cases — gap +14% vs unconstrained`
  - `instruction  strict 9/12, loose 11/12 — chattiness gap 2`

Tests: parse test for `--suite`; renderer summaries from canned rows (both gaps, counts printed); hash changes when the suite includes agentic and is stable otherwise.

Commit: `feat(bench): --suite agentic — the corpus-free probe suites wired end to end`

---

### Task 5: docs + live demonstration

CHANGELOG + IDEAS (part 3 v0 SHIPPED; remaining probes named with what they wait on). Live: `chekov capability bench --suite agentic --yes` against a launched candidate; paste the per-suite summary.

Commit: `docs(bench): changelog and status for probe suites v0`

## Self-Review
- §7.2 row 1 (tool_emit incl. abstention + missing-function) T1/T2/T4; row 2 (grammar_gap = forced − unconstrained, the anti-self-deception device) T2/T3/T4; row 6 (instruction, strict+loose separately, chattiness gap) T1/T2/T4. Composite scoring stays absent — §7.5 withholds it while any axis is missing, and most axes are.
- Types: `ToolCase`/`InstructionCase` (T1) → graders (T2) → forced_schema (T3) → orchestration (T4); `Grade` reused from the shipped fixture path.
