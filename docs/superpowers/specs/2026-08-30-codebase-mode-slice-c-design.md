# Codebase mode, slice C — `--judge`, the position-swapped binary tie-breaker

Date: 2026-08-30. Builds on slices A, B1 and B2 (`2026-08-29-codebase-mode-slice-a-design.md`,
`…-slice-b1-design.md`, `2026-08-30-codebase-mode-slice-b2-design.md`). This is the last
open item of the umbrella spec's slice 6 (`docs/capability-spec.md` §10, §11). Every
constraint §10 lists is carried here verbatim in intent; where this slice narrows or
departs from §10's wording, §9 below says so.

Status: DESIGN — awaiting human review. Nothing below is implemented.

## 1. Why this slice, and what it deliberately leaves out

Tiers 1–7 grade a fill by resemblance and by execution. `function_body` tasks get the
least of both: tiers 1–2 are skipped for them by design ("tiers 1-2 punish valid
alternatives", `ladder.rs`), tier 7 needs a covering test that most functions do not
have, and a correct alternative implementation of a body scores below a wrong one that
happens to reuse the gold's identifiers. The umbrella spec puts exactly one model call
into the whole feature for this gap: a small local judge answering one **binary**
question — *do these two bodies do the same thing here?* — twice, with the order
swapped, agreeing or abstaining.

Left out, and said in every report:

- **Fixture traps** (§10 case (a)). There is deliberately no compiled-in fixture and
  fixture-v1 is release-gated; the judge has nothing to judge there. The judge module
  is written against a span pair, not a codebase row, so the fixture can reuse it when
  it exists — but no fixture wiring ships here.
- **`in_file` and `cross_file_first` crossings.** §10 names function-body masks; the
  other task tiers keep tiers 1–2 and have the deterministic signal the judge exists to
  replace. Widening is a one-line change in the eligibility rule (§4) and a report
  change; it waits for evidence that it is needed.
- **Any composite score.** B2 left it out; C does not add it. §10's "capped at ≤5% of
  the composite" has no composite to cap: the judge is one more separately reported
  column that can never touch another column's number.
- **Co-resident judging.** The judge runs in its own phase after every candidate is
  down (§3). chekov manages one server; a second port is a different design.
- **Registering a judge from the command line.** `role = "judge"` is a `models.toml`
  field set by hand (§2); `pull` grows no flag.

## 2. Naming a judge

`ModelEntry` gains `#[serde(default, skip_serializing_if = "Option::is_none")] role:
Option<String>`; the only accepted value is `"judge"` and any other is refused at
registry load as `RegistryBadRole { name, role }` naming the key. A judge entry is
otherwise an ordinary entry: `pull` registers it, `run` can serve it, `list` shows a
`judge` marker. It is excluded from nothing else in this slice.

`capability bench` gains `--judge <NAME>` (`Option<String>`, `requires = "codebase"`).
Resolution happens at plan time, before any launch, and a judge that cannot serve is a
refusal — the run is not spent to report `N/A` at the end (§9 says why):

| condition | error |
|---|---|
| not in `models.toml` | `ModelNotRegistered` (existing) |
| registered without `role = "judge"` | `JudgeNoRole { name }` — "add `role = \"judge\"` to its entry" |
| same family as any candidate (§2.1) | `JudgeFamilyConflict { judge, candidate, family }` |
| named among `--models` | `JudgeFamilyConflict` with `candidate = judge` |
| a running server that bench did not start | `JudgeNeedsTheServer` — "bench never stops a server it did not start; stop it or drop --judge" |
| `--resume` of a run stamped with a different judge | `BenchStampMismatch` on the first differing judge field (existing path) |

### 2.1 Family

`general.architecture` is read from the judge's `first_shard` and from every
candidate's (`gguf::read_geometry`, as `explain` does). The **family key** is the
architecture string with a trailing `moe` removed: `qwen35moe` and `qwen35` are one
family (today's registry: ornith-1.5-35b-a3b, ornith-1.5-397b and qwen3.8-27b), while
`qwen4exp`, `gpt-oss` and `minimax-m2` are each their own. The rule is mechanical and
printed with the refusal; it is a floor against the documented self-preference and
sibling-preference effects, not a guarantee of independence, and the header (§6)
always names the judge so a reader can apply their own judgement.

With today's registry, `gpt-oss-120b` is the only entry that could take `role =
"judge"` against the ornith/qwen candidates; §10's recommendation is a 7–14B instruct
model of another family, which would be a new `pull`.

## 3. Lifecycle

The plan gains one step after the candidates: `judge <name>  launch + teardown  weights
X GiB`. `--dry-run` prints it and the estimate adds `+ judge: 2 orders × <crossings>
verdicts, ~<s> s each`. `needs_confirm` is already true for any launch.

Execution order with `--models a,b --judge j`:

1. candidate `a`: launch → measure → teardown → budget released (unchanged);
2. candidate `b`: the same;
3. **judge phase**: launch `j` once through `launch_candidate` (same preflight, flag
   hygiene and Metal residency), then for each run directory produced in steps 1–2,
   judge its eligible crossings (§4) and append the verdict rows to that run's
   `results.jsonl`; teardown `j` with the budget-release check.

One judge load serves every candidate. A judge phase that fails to launch is a
`ServerFailed`-class error after the candidate runs have landed on disk; every run is
resumable, and `--resume <RUN> --judge j` runs only the judge phase for that run
(§5). A single `UseRunning` step is refused with `--judge` (§2) — the judge phase
would have to stop it.

## 4. What is judged, and how

**Eligible crossing:** a stored codebase row with `tier == function_body`, a
non-empty prediction (a row nobody answered has nothing to judge), and — when the run
had `--allow-exec` — a tier-6 verdict other than `Value(0.0)` (a fill that did not
compile is decided; the judge never re-opens a deterministic result). A prediction
byte-identical to the gold after the tiers-1–4 trim is decided too: it records
`equivalent = true, decided_by = "identical"` without a call.

**One verdict = two calls**, both through chekov's own translator on the forced wire
(`runner::cross_forced`: Anthropic-shaped request → OpenAI `/v1/chat/completions`
with `response_format = json_schema`, `reasoning_format = deepseek`, greedy, seeded —
the exact mechanism the `grammar_gap` probe validated against thinking-prefill
templates on 2026-08-29). Order 1 shows the gold as A and the prediction as B; order 2
swaps them. Each call's schema is

```json
{"type":"object","properties":{"same_behavior":{"type":"boolean"}},
 "required":["same_behavior"],"additionalProperties":false}
```

so parsing cannot fail. `max_tokens` is `[bench] judge_max_tokens` (default 32 —
§10's 10–20-token verdict plus headroom for a template that opens a reasoning block);
a reply the grammar did not complete inside the budget is `skipped("reply truncated at
32 tokens")`, counted and printed, never a verdict.

**Agreement is the verdict; disagreement is an abstention.** `same_behavior` equal in
both orders → `equivalent: Some(bool)`; unequal → `equivalent: None` with
`decided_by = "swap disagreement"`. An abstention is a real row and counts against the
consistency rate (§5); it is never a fail and never a pass.

**The rubric** is one frozen `const` template in `src/core/bench/judge.rs`:

- system: the question, stated once — "A and B are two versions of the same span of
  Rust. Answer whether B would behave the same as A for every input, in this file, at
  this position. Reply with the JSON object only."
- user: the file path; the last `CONTEXT_BEFORE_LINES` (40) lines of the prefix and
  the first `CONTEXT_AFTER_LINES` (20) of the suffix, each in a fenced block; then
  `A:` and `B:` fenced blocks holding the two spans, each cut at `SPAN_MAX_CHARS`
  (4096) — the same cap for both sides, per §10's verbosity control. The prediction
  shown is the trimmed text tiers 1–4 grade, so A and B are already comparable in
  length.

`rubric_hash` = `sha256(template ‖ schema ‖ the three constants ‖ "judge-v1")[..12]`.
It goes into the stamp; a changed prompt is a changed instrument, never silently
mixed. There is no knob that changes the prompt.

## 5. Storage, resume, and the consistency floor

Verdicts are their own rows — the results file is append-only and the codebase row
was flushed long before the judge phase ran:

```rust
// store::Task gains `judge: Option<JudgeRow>`; suite = "judge", task_id = the
// crossing's task id, transport = Buffered.
pub struct JudgeRow {
    pub equivalent: Option<bool>,   // None = the two orders disagreed
    pub orders: [Option<bool>; 2],  // the raw answers, order 1 then order 2
    pub decided_by: String,         // "swap agreement" | "swap disagreement" | "identical"
    pub skipped: Option<String>,    // "reply truncated at N tokens" | "did not compile" | …
    pub judge_secs: f64,
}
```

`TaskKey::buffered("judge", id)` makes `--resume` skip judged crossings; a resumed run
whose stamp names a different judge is refused before anything is loaded. The stamp
gains `judge: Option<JudgeStamp { model, quant, revision, arch, rubric_hash,
max_tokens }>`, and `first_mismatch` compares it as one field, `judge`, after
`corpus_id`.

**Consistency** is computed on read, never stored: `agreements / (agreements +
disagreements)` over rows with two answers. Below `[bench] judge_min_consistency_pct`
(default 70) the column is **voided** for that run — the cell prints
`equiv voided (swap consistency 58% < 70%)` and `compare` treats it as absent. The
rows stay: a voided instrument's raw answers are still evidence about the judge.

## 6. Report

The `function_body` line gains one cell after `test`, and the header one clause:

```
codebase: … ; judge: gemma-3-12b-it (Q8_0@1a2b3c4d5e6f, gemma3) rubric 9f8e7d6c5b4a, swap consistency 83% (5 of 6)
function_body   ident_f1 0.70  parse 0.83  symbols 0.85 (scored at run time)  compile 0.67 (n=6)  test 0.50 (n=2 of 6 had a covering test)  equiv 0.60 (n=5 judged of 6; 1 undecided)   (n=6)
             judge: 1 identical, 4 called, 1 undecided, 0 skipped; 2.1 s median per verdict
```

- `equiv`'s mean is over rows with `equivalent: Some`; `identical` rows count as
  `true`. `n judged` excludes undecided and skipped; both are named in the parenthetical
  and tallied by reason in the trailer, like the exec skips.
- Other tier lines print no `equiv` cell. Without `--judge`: the trailer reads `judge
  skipped: --judge not given`. A run whose judge phase never ran (crashed before it,
  resumable): `judge: 0 of 6 crossings judged — resume with --judge <name>`.
- `compare` gains an `equiv` row under `function_body` with the same paired sign test as
  every other tier, over crossings both runs judged. When the two runs' `judge` stamps
  differ (either absent, or a different model/revision/rubric) the row is
  `equiv: not compared (judge differs: a=<…> b=<…>)` and nothing else in the
  comparison changes — a run judged today stays comparable on tiers 1–7 with the 24
  runs already under `eval/`.

## 7. Errors and edge cases

- `JudgeNoRole`, `JudgeFamilyConflict`, `JudgeNeedsTheServer`, `RegistryBadRole` — new,
  in `error.rs`, each with a remediation sentence.
- The judge server answers a non-2xx: `UpstreamRefused` for that crossing →
  `skipped("judge refused: <the server's words>")`; the phase continues. The judge
  server dies: the phase stops with the usual `EndpointDown`, rows so far intact.
- A run with zero eligible crossings (no `function_body` tasks answered) still launches
  the judge only if another run in the same invocation has any; otherwise the phase is
  skipped and the trailer says `judge: nothing eligible`.
- `--judge` without `--codebase` is a clap error (`requires`).
- Eligibility reads the stored rows, so a run recorded before slice C can be judged
  with `--resume <RUN> --judge j` — the stamp gains its `judge` field on resume only if
  it had none; the head is rewritten with the field added, and nothing else changes.

## 8. Tests

- Registry: `role = "judge"` round-trips; `role = "candidate"` is refused naming the key;
  an entry without the field loads as `None`.
- Family key: `qwen35moe` ≡ `qwen35`; `qwen4exp` ≠ `qwen35`; the conflict error names
  judge, candidate and family; a judge listed in `--models` conflicts with itself.
- Rubric: the hash is stable across runs and changes when the template, schema or a
  constant changes; both spans are cut at the same cap; the prefix tail / suffix head
  line counts.
- Verdict assembly (canned upstream, no server): agreement → `Some(bool)`; disagreement
  → `None` with `decided_by = "swap disagreement"`; a truncated reply → the skip string
  with the budget in it; `identical` short-circuits with no request sent; a
  non-compiling row is skipped without a request; only `function_body` rows are eligible.
- Wire: each order's request is Anthropic-shaped, carries the schema on the forced
  wire, and swaps A and B exactly.
- Consistency: 4 of 6 → 67% voids the column at the default floor; 5 of 6 does not;
  the void string carries both numbers.
- Store: `JudgeRow` round-trip; a pre-C row loads with `judge: None`; `TaskKey` resume
  skip; stamp round-trip with and without `judge`; `first_mismatch` names `judge`.
- Report and compare: the exact cell, header clause and trailer strings above; the
  `not compared (judge differs…)` row leaves the tier rows unchanged.
- Live (PR body): `--codebase . --allow-exec --judge <j>` on this repo with
  `ornith-1.5-35b-a3b`, the swap-consistency rate and the `equiv` cell, plus a `compare`
  against a pre-C run showing the `not compared` row.

## 9. Departures from the umbrella spec's §10 wording, stated

1. **Plan-time refusal instead of `N/A`.** §10 says an unregistered judge or a family
   conflict reports `N/A (judge unavailable: …)`. Those are both known before the first
   launch; spending a 40-minute run to say so at the end is the failure mode the
   loud-failure creed exists to prevent. Run-time failures (a refused reply, a
   truncated reply, the consistency floor) are reported as §10 says.
2. **No composite, so no 5% cap.** The judge is a column, not a weight (§1).
3. **`function_body` only** (§1) — §10's case (b) exactly; case (a) has no fixture.
4. **A verdict is `same_behavior`, not `winner`.** There is no second candidate in a
   codebase row; the pairwise form is gold-vs-prediction, and the swap still applies.

## 10. Open questions for the human

1. **Which judge?** Approve a `pull` of a 7–14B instruct model outside the
   Qwen family (Gemma 3 12B or Phi-4 14B are the §10 evidence base), or use
   `gpt-oss-120b` (arch `gpt-oss`, already registered) as the first judge and accept
   that it is a thinking model whose `<think>` span competes with the 32-token budget.
2. **Budget.** Is `judge_max_tokens = 32` the right default, or should the default be
   §10's literal 20 with the truncation counter as the evidence for raising it?
3. **Eligibility width.** Keep `function_body` only, or judge every crossing whose
   exact tier is below 1.0?
4. **`role` by hand.** Acceptable for this slice, or should `pull` take `--role judge`?

## 11. Files

`src/core/bench/judge.rs` (new: rubric + hash, family key, eligibility, request pair,
verdict assembly, consistency), `src/core/bench/store.rs` (`JudgeRow`, the cell, header
clause, trailer), `src/core/bench/stamp.rs` (`JudgeStamp`, the compare field),
`src/core/bench/lifecycle.rs` (the judge step and its estimate), `src/core/bench/compare.rs`
(the `equiv` row and the `not compared` line), `src/core/registry.rs` (`role`),
`src/core/config.rs` (`judge_max_tokens`, `judge_min_consistency_pct`),
`src/commands/capability.rs` (`--judge`, resolution, the judge phase),
`src/error.rs` (four errors), `README.md`, `CHANGELOG.md`, `IDEAS.md:134`, a pointer from
`docs/capability-spec.md` §10/§11. Estimated ~900 LOC including tests; well past the
"ask before >5 files" line, which is why this document stops here.
