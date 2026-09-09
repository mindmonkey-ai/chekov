# `capability bench` — the `tool_loop` probe: a canned read→edit→verify loop — design

Date: 2026-09-06. Status: approved in chat 2026-09-09; this document is the
binding spec. Implements spec §7.2 row 4 (`tool_loop`, N = 6) of the
approved capability entry in IDEAS.md ("Machine capability scan, frontier
graph, recommendations and agent bench", 2026-08-25). Touches ~13 files, so
AGENTS.md's ">5 files — ask first" rule was satisfied by the same approval.

## 1. Purpose, and the evidence

Every agentic probe chekov ships today is single-turn: `tool_emit` asks for
one call, `grammar_gap` forces its shape, `instruction` grades one reply.
Claude Code is none of those things. It is a loop — read a file, edit it,
run the tests, read the failure, edit again — and the model's fitness as a
backend is whether that loop reaches a terminal state, not whether one call
is well-formed.

The single-turn axis has stopped separating the models chekov benches. The
2026-09-01 and 2026-09-02 face-offs scored `tool_emit` 8/10, 8/10, 10/10 and
9/10 across four models, `grammar_gap` 6/7 or 7/7 everywhere. That matches
the field: the 2026-07 cross-benchmark corpus puts single-turn function
calling at a 0.97 frontier ("near saturation") while stateful multi-turn
suites — BFCL-v4 Multi-Turn, τ²-Bench, Terminal-Bench 2.x — are where models
still spread. The spec anticipated this in §7.2: `tool_loop` is "multi-turn
read→edit→verify against an in-process mock tool server with fully canned
responses; reached terminal state within K turns; deterministic because the
environment is canned."

This design ships exactly that: six cases, a canned in-process environment
whose every reply is a pure function of the case and the calls so far, both
transport doors (the streamed one is the door Claude Code takes, and
multi-turn `tool_use` reassembly is where a translator defect would hide),
and a grade that is a terminal-state check, never a judgment of the path.

## 2. Command surface

Unchanged. `capability bench --suite agentic` and `--suite all` run the
loop cases after `tool_emit` and `instruction`, through both doors, exactly
as the other agentic cases do. `--resume` skips recorded rows by the
existing `TaskKey` (`suite = "tool_loop"`, `task_id`, `transport`).
`--dry-run`'s estimate grows by the loop's upper bound (§7).

One new config key, `[bench] tool_loop_max_turns` (default 8) — the K of
the spec. No CLI flag: a per-run turn budget is a knob nobody has asked
for, and the record stamps the value it ran under anyway (§8).

## 3. The cases (`agentic_v0.toml`, six `[[tool_loop]]` entries)

Each case is a small canned repository — two to four short Rust files — a
task prompt, a tool palette drawn from the same five tools `tool_emit`
already uses (`read_file`, `list_dir`, `grep`, `edit_file`, `run_tests`),
and a **goal**: the terminal state that counts as done. The palette is the
case's own, as in `tool_emit`, so a model that fabricates a tool is graded
as fabricating, not as using something the case forgot to offer.

| id | shape | what it discriminates |
|---|---|---|
| `tl-001` | read → edit → run_tests. A constant in the file named by the prompt has the wrong value. | Can the loop close at all: read before editing, edit exactly, verify. |
| `tl-002` | grep → read → edit. The prompt names a symbol, not a file; two files exist, one holds the definition. | Finds the definition rather than guessing a path. |
| `tl-003` | edit → run_tests (FAIL) → read the failure → edit again. The obvious first edit is a near miss: the test message names the actual expectation. | Reads a tool result and changes course — the loop's whole point. |
| `tl-004` | list_dir → read two similar files → edit the right one. Both files define a function of the same name; only one is `pub`, and the prompt says which module. | Disambiguation with context, not first-hit editing. |
| `tl-005` | The prompt names a file that does not exist. Goal: no file changes, and the final reply names the missing path. | Abstention inside a loop: stops honestly instead of inventing a file to fix. |
| `tl-006` | read A → read B → edit B. A's doc-comment states an invariant; B violates it; the prompt asks to make B honour A. | Cross-file integration, the tier where codebase mode already shows models separating. |

The TOML shape, one case:

```toml
[[tool_loop]]
id = "tl-001"
prompt = "MAX_RETRIES in src/config.rs must be 5, not 3. Fix it and run the tests."
[[tool_loop.files]]
path = "src/config.rs"
text = "pub const MAX_RETRIES: u32 = 3;\n"
[tool_loop.goal]
kind = "edited"                          # or "unchanged"
file = "src/config.rs"
contains = "pub const MAX_RETRIES: u32 = 5;"
tests_fail = "test retries_default ... FAILED: expected 5, got 3"
# kind = "unchanged" cases carry `reply_mentions = "src/legacy.rs"` instead
[[tool_loop.tools]]
name = "read_file"
description = "Read a file from the repository and return its contents."
input_schema = '{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}'
# … edit_file, run_tests — the same definitions tool_emit already carries
```

`probeset.rs` gains `LoopCase { id, prompt, files: Vec<CannedFile>, goal:
Goal, tools: Vec<ToolDef> }` with `Goal` an enum (`Edited { file,
contains, tests_fail }` / `Unchanged { reply_mentions }`, tagged by
`kind`). Validation at load, loud like the rest of the set: every `goal.file`
names a canned file; every goal `contains` is NOT already present in that
file (a goal met before the first turn is a broken case, not a free pass);
the palette carries every tool the goal's kind needs (`edit_file` for
`edited`; `run_tests` only when `tests_fail` is set); ids are unique across
the whole set (the existing `validate_ids` extended to the third array).

**Comparability.** The TOML text is what `probeset::content_hash` hashes,
so adding these cases changes `prompt_set_hash` and `compare` will refuse a
new agentic run against any stored one, naming that field. That is the
set's documented contract ("editing a case makes old runs incomparable BY
CONSTRUCTION — grow toward the spec's counts, never edit in place"): the
existing runs stay comparable among themselves; the CHANGELOG says so.

## 4. The environment (`src/core/bench/toolloop.rs`, new)

`ToolEnv` holds the case's files as `BTreeMap<String, String>` plus the
goal, and answers one call at a time. Every answer is a pure function of
(case, state) — no clock, no randomness, no filesystem — which is what makes
the probe deterministic for a deterministic model (sampling is pinned greedy
and seeded on the wire, as for every probe):

| tool | answer |
|---|---|
| `read_file {path}` | the text, or `no such file: <path>` |
| `list_dir {path}` | the entries directly under it, one per line, or `no such directory: <path>` |
| `grep {pattern, path}` | `file:line: text` for every line containing the pattern (plain substring, not regex — the prompt never asks for one), or `no matches` |
| `edit_file {path, old, new}` | exactly one occurrence of `old` → replaced, `edited <path>`; zero → `old text not found in <path>`; N > 1 → `old text occurs N times in <path>; make it unique` |
| `run_tests {filter}` | `ok. 1 passed` when the goal is met, otherwise the case's fixed `tests_fail` line — the same words every time, never the answer |

`edit_file`'s three-way contract is Claude Code's own `Edit` tool contract
on purpose: uniqueness failures are the edit defect an agent backend must
recover from. A call naming a tool outside the palette, or with arguments
that do not deserialize to the tool's required keys, is not answered — it
ends the loop as `FabricatedTool` / `MalformedCall` (§6).

## 5. The driver

`toolloop::drive(cross, case, max_turns) -> Result<LoopOutcome, ChekovError>`
where `cross: &mut dyn FnMut(&HttpRequest) -> Result<Turn, ChekovError>` is
the door — `commands::capability::agentic_cross` wrapped, so the driver
lives in core with no knowledge of transports and is testable with
`runner`'s existing `CannedUpstream`. `Turn` is the Anthropic body plus the
crossing's `Option<Timings>`.

Per turn:

1. Build the request (`probes::loop_probe(case, &transcript)`): system =
   the fixed loop instruction (a constant, hashed like `THROUGHPUT_PROMPT`
   into nothing new — it is part of the TOML-adjacent text, so it goes in
   the TOML as a top-level `loop_system` string to keep "the set is the
   hash" true); `tools` = the palette; `messages` = the transcript;
   `max_tokens` 512.
2. Cross. Read the reply's `tool_use` blocks as `(id, name, input)` — a
   sibling of `grade::tool_uses` that keeps the block id, because
   `tool_result.tool_use_id` must echo it.
3. **No tool_use in the reply** → the model has ended its turn. Evaluate the
   goal → `GoalMet` or `GoalUnmet`. A `stop_reason` of `max_tokens` here is
   `Truncated`, never a pass: a reply the agent could not finish is a
   defect, not a decision.
4. **tool_use present** → execute each in order against `ToolEnv`; append
   the assistant message (the reply's content blocks verbatim — text and
   tool_use together, which is what Claude Code sends back) and one user
   message of `tool_result` blocks, one per call, in call order. The
   translator already maps both (`tool_result_becomes_a_separate_tool_role_message`,
   `tool_use_becomes_a_tool_call_with_stringified_arguments`).
5. Turn `max_turns` reached with the model still calling → `TurnsExhausted`.

`LoopOutcome { end: LoopEnd, turns: u32, tool_calls: u32, measure: Measure }`.
The measure is the loop's own: one decode and one prefill sample per timed
turn, `prompt_n` of the deepest turn, `cache_n` the max seen, drafts summed;
an untimed door (a foreign run's buffered door) leaves it empty via the
existing `codebase::run::empty_measure()`. `warmup_dropped` is 0 — nothing
here is a repetition.

## 6. Grading

`LoopEnd` is the grade, exhaustively:

| end | grade | row reason |
|---|---|---|
| `GoalMet` | PASS | — |
| `GoalUnmet` | FAIL | `stopped with the goal unmet after N turns: <what the goal wanted>` |
| `TurnsExhausted` | FAIL | `no terminal state in K turns (M tool calls)` |
| `Truncated` | FAIL | `final reply hit max_tokens` |
| `FabricatedTool` | FAIL | `called '<name>' — not in this case's palette` |
| `MalformedCall` | FAIL | `'<name>' called without <key>` |

For `Goal::Edited` the goal is met when the named file contains the
`contains` text. For `Goal::Unchanged` it is met when no file differs from
the canned set AND the final reply's text names `reply_mentions` (a
substring, case-insensitive, like `instruction`'s `contains` check). A
crossing `Err` mid-loop is a row failure through `append_probe`'s existing
swallow — a foreign run's missing-timings recast (`row_outcome`) applies
unchanged.

Nothing grades the path. A model that reads three files before the one
edit is as right as one that reads one; a model that reaches the goal in
two turns and one that needs six both PASS, and the turn count is printed
beside them, not folded into the grade.

## 7. Report, compare, estimate

`store.rs`: `TaskRow` gains `#[serde(default, skip_serializing_if =
"Option::is_none")] pub tool_loop: Option<LoopRow>` with `LoopRow { turns,
tool_calls, end: LoopEnd }` (`LoopEnd` serialized `snake_case`); every row
on disk loads as `None`. `SCHEMA_VERSION` stays 1 — additive optional, the
same move slice C made for `judge`. `"tool_loop"` joins `AGENTIC` (its
failures print through `agentic_fail_line`) and `PAIRED` (both doors, so an
asymmetry between them is named like the others). One summary line per
door:

```
tool_loop    4/6 reached   turns 2/3/6 (min/median/max over reached)
tool_loop    3/6 reached   turns 2/4/5 (min/median/max over reached)  [streamed]
```

The runtime discrimination note fixture mode already prints applies here
verbatim: when every candidate in a `--models` run reaches 6/6 or 0/6, the
line ends `— not discriminating on this candidate set`.

`compare.rs`: `agentic_totals` gains `tool_loop_totals(&sides, transport)`
for both doors (label `tool_loop` / `tool_loop [streamed]`), counted by the
store's `Tally` like `tool_emit` so the comparison cannot count differently
from the report; the per-case disagreement lines come for free from
`compare_agentic` once the suite is in `AGENTIC`.

`capability.rs`: `run_agentic` runs `set.tool_loop` after the instruction
cases inside each door's pass; `agentic_estimate_secs` adds the upper bound
`2 doors × cases × max_turns × 8 s` (768 s at the defaults) and the
`--dry-run` plan says "up to" for it, since a loop that closes in two turns
costs a quarter of that.

## 8. Configuration and stamp

`[bench] tool_loop_max_turns = 8` on `BenchSection` (serde default 8;
`config.example.toml` gets the line and a one-clause comment). The value
rides into the run as part of the agentic component of `prompt_set_hash`
(`agentic|<content>|turns=<K>|seed=<seed>`): two runs judged under
different turn budgets measured different tasks and must refuse to compare
by name, exactly as a changed depth list does for throughput. A stored
throughput-only run's hash is untouched (`Suite::Throughput` keeps
`prompt_set_hash` as it was).

## 9. Errors

No new `ChekovError` variants. Probe-set defects surface through the
existing `invalid(...)` refusal at load; wire and translation failures are
the existing crossing errors, recorded per row. The one place a new
message appears is the row reason table in §6, which is row text, not an
error.

## 10. Testing

`tests/**` is write-protected under pushkin, so everything is an inline
`#[cfg(test)]` module beside the code it tests, in the style of the
existing suites:

- `probeset.rs`: the shipped set parses with six loop cases; a goal whose
  `contains` is already present is refused naming the case; a goal file not
  in the case's files is refused; a palette missing `edit_file` on an
  `edited` goal is refused; duplicate ids across the three arrays are
  refused; `content_hash` changes when a loop case changes.
- `toolloop.rs` (environment): `read_file` on a missing path answers the
  fixed sentence; `edit_file` with a unique `old` replaces and later reads
  see it; zero and many occurrences answer their sentences and leave the
  file untouched; `grep` is substring, not regex (`.` matches a literal
  dot only); `run_tests` answers `tests_fail` verbatim before the goal and
  `ok. 1 passed` after; two environments fed the same call sequence hold
  identical state.
- `toolloop.rs` (driver), with a scripted fake door that returns canned
  Anthropic bodies in sequence: read→edit→stop reaches `GoalMet` in three
  turns with the transcript carrying `tool_result` blocks whose ids echo the
  `tool_use` ids; a body that calls an unknown tool ends `FabricatedTool`
  after that turn; a body with no `tool_use` and the goal unmet ends
  `GoalUnmet`; a `stop_reason` of `max_tokens` ends `Truncated`; a door
  that keeps calling ends `TurnsExhausted` at exactly K; the measure holds
  one sample per timed turn and `prompt_n` of the last.
- `probes.rs`: the loop request carries the palette as Anthropic `tools`,
  the system text, and the transcript in order.
- `store.rs`: rows written before the field load with `tool_loop: None`; the
  summary line renders counts and the min/median/max; a `[streamed]` line
  appears only when streamed rows exist; the FAIL line carries the §6
  reason; the not-discriminating clause appears on 6/6 and 0/6.
- `compare.rs`: two runs' `tool_loop` totals appear per door, and a case
  passed by one run only is listed with the losing reason.
- `capability.rs`: the estimate adds `2 × 6 × 8 × 8` seconds with the set
  at its shipped size; `--resume` skips a recorded `tool_loop` row and not
  its other-door twin.
- `config.rs`: `tool_loop_max_turns` defaults to 8 and parses an override.

Acceptance, live: re-run `--suite agentic` on `ornith-1.5-35b-a3b`,
`ornith-1.5-9b`, `qwen3.5-9b` and `qwen3.8-9b-distill` (the four whose
single-turn scores tied) and publish the spread in the CHANGELOG entry. If
all four land on the same count, the probe is the problem and the entry
says so — the same rule fixture-v1's release gate applies to itself.

## 11. Out of scope

- A real filesystem, a real shell, or executing anything: the environment
  is canned by design (spec §7.2), and `--allow-exec` stays the single gate
  on code that runs.
- Per-case turn budgets, `tool_choice` forcing, or a grammar-forced arm:
  the loop's axis is reaching the goal unconstrained.
- Judge involvement, fixture-mode loops, non-Rust canned repositories.
- Grading the path (turn efficiency as a score). Printed, never scored.
- Growing the other agentic arrays toward 30/40 — a separate change with
  its own comparability break.

## 12. Files

`src/core/bench/agentic_v0.toml` (six cases, `loop_system`),
`src/core/bench/probeset.rs` (`LoopCase`, `Goal`, validation),
`src/core/bench/toolloop.rs` (new: `ToolEnv`, `drive`, `LoopOutcome`),
`src/core/bench/probes.rs` (`loop_probe`), `src/core/bench/grade.rs`
(`tool_use_blocks` with ids), `src/core/bench/store.rs` (`LoopRow`,
consts, summary line), `src/core/bench/compare.rs` (`tool_loop_totals`),
`src/commands/capability.rs` (`run_loop_case`, estimate),
`src/core/config.rs` + `config.example.toml` (`tool_loop_max_turns`),
`README.md` (the `capability bench` row's `agentic` clause and the
`[bench]` table), `CHANGELOG.md`, `docs/capability-spec.md` (§7.2 row 4
status line). Thirteen files: approval requested for the count as well as
the design.
