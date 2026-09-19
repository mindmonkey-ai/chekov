# IDEAS — chekov

New capability ideas live here, not in code (charter N13). An idea is a one-line
proposal with a one-line rationale; it becomes work only after the human
approves it and it is moved into a phase/task. Nothing here is implemented until
it is approved.

<!-- Add new ideas below. Format:
## <short title>
<what + why, one or two lines>
Proposed <date> — status: OPEN / APPROVED / DEFERRED
-->

## Model-fit sizing (2026-08-21)
A reference for deciding whether a GGUF fits before registering it in
`models.toml`. Mirrors chekov's `verdict_for` math (`TIGHT_FRACTION_PCT = 85`,
config MB treated as MiB, weights-only vs RSS+KV caveat) and the machine's
182.62 GiB wired budget. See the `chekov-development` skill
`references/model-fit-sizing.md`.
Proposed 2026-08-21 — status: documented.

## CLI evolution recipes (2026-08-21)
Worked recipes for flipping a flag's default while keeping a hidden back-compat
alias, and folding an overlapping subcommand into another as a mode flag. See
the `chekov-development` skill `references/cli-evolution.md`.
Proposed 2026-08-21 — status: documented.

## Pin the llama.cpp engine to a ref (2026-08-25)
`setup` clones llama.cpp master with no ref and `update --engine` fast-forwards
it, so the engine chekov builds is whatever upstream HEAD was that day. Model
weights are revision-pinned; the binary that runs them is not. Proposal: an
`[engine] git_ref` config key, `git fetch origin <ref>` + `checkout --detach
FETCH_HEAD` instead of `pull --ff-only`, and `--branch <ref>` on the clone.
Deferred from the provenance work, which only records the built commit — pinning
adds config surface and changes what `setup` does on every machine.
SHIPPED 2026-08-29 as `[engine] git_ref` (branch, tag, or commit). Absent means
today's behaviour exactly — nothing changes until a machine opts in. Pinned:
`git fetch origin <ref>` + `checkout --detach FETCH_HEAD` (never a pull; no
`--branch` on the clone, so a sha is a valid pin); `update --engine` prints
`(pinned to <ref>)`. A ref starting with `-` or containing whitespace is
refused at config load naming the key — git would read the first as an option.
Why it earned its place: on 2026-08-28 the engine had to be moved to a
master-plus-one-cherry-pick branch by hand to gain `qwen4exp`, and `update
--engine` on the old fix branch reported `dda1b0d67 → dda1b0d67`.
Proposed 2026-08-25 — status: SHIPPED

## Verify the engine binary after building it (2026-08-25)
`update --engine` never runs the binary it just built: a llama.cpp change that
breaks the build's output surfaces later, as a failed `run`. Proposal: a fourth
`EngineStep` running `llama-server --version`, so a bad build fails as
`EngineStepFailed` naming the step, and `--dry-run` prints it like the others.
Auto-rollback was considered and rejected — `git checkout <before>` plus a full
rebuild is a multi-minute silent side effect, the opposite of the loud-failure
creed. With the commit now recorded in `logs/chekov.engine`, a manual revert has
something to name.
SHIPPED 2026-08-29: `setup_steps` ends with `<engine>/build/bin/llama-server
--version` as its own `EngineStep` ("verify the built llama-server runs"), on
both the pinned and the unpinned path; it prints under `--dry-run` and fails
as `EngineStepFailed` naming the step. No rollback, as decided above.
Proposed 2026-08-25 — status: SHIPPED

## Replace hf-hub with the ureq already in the tree (2026-08-25)
`hf_hub` appears at exactly one call site (`core/hub.rs:363`) and its three-call
surface is `new()` / `model()` / one `download_file()` builder, yet it pulls 225
of the crate's 256 transitive dependencies — including tokio, reqwest, hyper and
the xet stack. chekov already queries the HF API over ureq at `hub.rs:126`.
Against it: hf-hub provides resume and Xet-accelerated transfer, which matter for
100+ GB pulls; hand-rolling means Range-request resume and losing Xet.
DONE (commit 0bc0b0d, "drop hf-hub for a streaming ureq download — 256 crates
to 66"): `hub::fetch_to` streams each file over ureq into a `.part` sibling
and renames it into place; Xet-backed repos redirect to a CAS bridge that
serves plain HTTPS with a content-length, verified against
unsloth/MiniMax-M2.7-GGUF. Range-request resume was NOT built — an interrupted
shard restarts from zero. Recorded here 2026-08-29 because the entry still
read OPEN.
Proposed 2026-08-25 — status: DONE (partial-shard resume SHIPPED 2026-08-29)

## `chekov stop --if-running` (2026-08-26)
A teardown script cannot call `stop` idempotently: stopping an already-stopped
server exits 1, and every error class shares exit 1, so the script cannot tell
that benign case from a real failure. Proposal: an opt-in `--if-running` flag
that prints "nothing to stop" and exits 0. Opt-in, not the default — a silent
no-op by default would weaken the loud-failure creed. A new flag is new
capability, so it waits here.
SHIPPED 2026-08-29 exactly as proposed: the flag covers only "no pidfile at
all"; a stale pidfile is still cleaned and reported (already exit 0), and a
stop that fails still fails, flag or not.
Proposed 2026-08-26 — status: SHIPPED

## `update --accept-license-change` for unattended runs (2026-08-26)
`update --model` cannot run in cron once a vendor changes their license text:
the STOP-4 gate needs a tty. The confirmation now says so plainly rather than
reporting a phantom decline, which is the honest half of the fix. Whether to
ALLOW unattended acceptance is a separate policy call — update.rs:147 says
"STOP-4: explicit confirmation, never assumed", and no evidence in the repo
shows anyone running `update --model` unattended. Only add the flag if that
changes.
Proposed 2026-08-26 — status: OPEN

## A live context-window check in `doctor` (2026-08-27)
`doctor`'s fifth row compares `models.toml` to `config.toml` and nothing else,
so it is the one row that can report PASS while the server is down. It is now
named "context floor (config, not the server)" so it cannot be misread as
evidence of health — but nothing yet verifies the context the SERVER actually
loaded, which can differ from the registry's intent indefinitely (the
`status-reports-registry-not-server` finding). Proposal: a sixth row probing
llama-server's `/props` for `n_ctx` and comparing it to the effective
`ctx_size`. A new check is new capability, and it touches every "five checks"
doc surface, so it waits here.
SHIPPED 2026-08-29 as the sixth row, "context loaded (server /props)",
reusing the bench's `runner::assert_props_ctx` verbatim (doctor and bench
cannot disagree). `/props` is behind `--api-key`, so doctor passes
`serve::get_bearer` through the bench's `PropsFetch` seam — no change to the
`HttpClient` trait or any fake. Unreachable = FAIL, like the other server
rows. Every "five checks" surface now says six.
Proposed 2026-08-27 — status: SHIPPED

## Machine capability scan, frontier graph, recommendations and agent bench (2026-08-25)
`chekov capability {scan,graph,recommend,explain,bench,compare}` — probe the machine
(sysctl / ioreg / `llama-server --list-devices` / df+mount), render an ASCII+SVG frontier of
model x quant x ctx with fits/tight/exceeds and predicted-vs-measured tok/s, recommend
candidates with the sizing math shown, and benchmark them through chekov's own
Anthropic->OpenAI translator against a built-in fixture or the user's own repo.
Motivated by a measured defect: `checks::effective_wired_mb` reports 196608 MiB on this
machine where the engine reports 228065 MiB — chekov understates its own budget by 30.7 GiB.
Verified 2026-08-27: `./llama.cpp/build/bin/llama-server --list-devices` prints
`MTL0: Apple M3 Ultra (228065 MiB, 228064 MiB free)`.
Supersedes the arithmetic in `references/model-fit-sizing.md` (see "Model-fit sizing", above).
`--codebase` leftover: a crash between the worktree add and its removal leaves
`<eval>/.scratch/codebase-tree-<head12>` on disk, registered in the target repo.
It is hidden, so nothing that enumerates the eval dir reads it, and the next run
removes and re-adds it itself — the manual cleanup, if you want the space back
now, is `git worktree prune` in the target repo plus deleting that directory.
Round check 2026-09-06: every slice-6 item in spec §12 has shipped (worktree
isolation, `--allow-exec`, the leakage filter, HEAD-seeded sampling, `/infill`
N/A, `--svg`, `--judge`), so what this entry still owes is §7.2's deferred
probes and the seed counts. Ordered by what the evidence says pays next:
`tool_loop` — single-turn `tool_emit` is saturating here (8/10, 8/10, 10/10,
9/10 across the four face-off models) and in the field (a 0.97 frontier on
BFCL-style single calls against a real spread on BFCL-v4 Multi-Turn and
tau-bench); design APPROVED 2026-09-09 —
`docs/superpowers/specs/2026-09-06-tool-loop-probe-design.md` is binding. Then
`long_ctx_trace`, which only pays once a run reaches past the 16K depth the
sweep stops at today (see "tune judges at 4096 tokens" below) — at the
current depths its recommended `ctx_size` would read "≥16384, the largest
measured", which recommends nothing. `hallucination` is largely covered by
codebase tier 5 (repo-symbol existence) and `diff_fidelity` measures an edit
shape (unified diffs) Claude Code does not emit — both stay deferred on
purpose. `think_leak` still waits on §13 Q5. The agentic set stands at
10/7/12 of the spec's 30/30/40; growing it breaks comparability with every
stored agentic run by construction, so it is its own decision.
Proposed 2026-08-25 — status: **slices 1-3 SHIPPED; slice 4 SHIPPED without the compiled-in seed catalog (human's call 2026-08-27: a vendored list rots; --refresh is the discovery layer); slice 5 harness SHIPPED 2026-08-27, upgraded 2026-08-28 with the §7.4-§7.5 stamp + JSONL store (17-field stamp, first-differing-field compare refusal, --resume, pinned sampling); slice-5 gap part 2 (per-candidate lifecycle §7.3: --models, flag hygiene, Metal env, teardown+release check, confirm/dry-run, cache_n) SHIPPED 2026-08-28; part 3 (probe suites §7.2) v0 SHIPPED 2026-08-28 (--suite agentic: tool_emit/grammar_gap/instruction seed set, growing toward 30/40; deferred: diff_fidelity+tool_loop+long_ctx_trace+hallucination need the §8/§9 corpora, think_leak waits on §13 Q5); slice-5 "`--metric tok-s` upgrades from predicted to measured" SHIPPED 2026-08-28 (fixed bands, deepest-depth median, exact-match + stale footer); fixture-v1 content release-gated; slice 6 OPEN (`--svg` SHIPPED 2026-08-28; --codebase slice A SHIPPED 2026-08-29 (Rust, same-file, tiers 1-5); `#[cfg(test)]` rule amended 2026-08-29 (items elided, file kept); slice B1 SHIPPED 2026-08-29 (cross_file_first, input_extra, two arms and the measured context lift; quota 12/6/6, corpus_id changed); slice B2 (exec tiers behind --allow-exec) SHIPPED 2026-08-30; slice C (--judge) SHIPPED 2026-08-30 (gpt-oss-20b recommended; probe in the spec §3.0))**

## Long-context trace checks: opt-in scope decision (2026-09-10)

**Question:** Should the next benchmark check whether a model can follow two
linked facts in a long prompt, and may its implementation touch more than five
files? The earlier tool-loop priority has shipped. This is the next ordered
probe in the capability entry above; tuning's deep-context measurement and
fresh-prefill live acceptance have also shipped.

**Ruling APPROVED 2026-09-10 (human approval in chat):** Add an explicit
`--long-ctx-trace <LENGTHS>` option to `capability bench`, for example
`--long-ctx-trace 4096,16384,65536,131072`. Existing benchmark commands keep
their current work and prompt hashes when the option is absent. No dependency,
new configuration file, registry write, or default deep sweep is proposed.

The approved implementation:

- Generate four deterministic two-hop cases at each requested length, with
  the linked facts separated and their positions varied through a seeded
  corpus. A four-length run has sixteen cases through each of the buffered
  and streamed transports. Distractors must prevent a single lookup or a
  repeated answer from passing without following the chain.
- Grade the visible answer by exact match, recording failures, truncation,
  unavailable measurements, and transport disagreements explicitly. Requested
  lengths are estimates until checked against server-reported prompt tokens;
  the report must distinguish requested from observed length and must never
  claim that a silently truncated prompt tested the requested context.
- Report accuracy by length and transport. Recommend only a fully measured,
  contiguous tested range holding at least 90% on both transports: with four
  cases, all four must pass. Missing, unavailable, duplicate, or failed cases
  cannot establish a passing range. A pass at the largest tested length is a
  lower bound on tested ability, never a discovered model maximum. Print the
  recommendation; never change `models.toml`.
- Include lengths, generator version, sampling seed, and answer budget in the
  opted-in prompt identity. Resume must skip the exact recorded cases, and
  comparisons must refuse different trace workloads while old runs retain
  their existing loading and comparison behavior.
- Show the extra crossings and length-dependent prefill cost in the plan and
  `--dry-run` before inference. Use the existing server ownership and context
  checks; an unsupported length must be named, not silently shortened.

**Approved scope:** More than five files, limited to benchmark generation,
CLI orchestration, run recording/reporting/comparison, focused tests, existing
user documentation, and generated completions. Implementation lives in
`src/core/bench/longctx.rs`, with CLI orchestration, runner translation helpers,
run header/row persistence, comparison, and benchmark module registration.
Existing record fixtures gained absent trace fields. The proxy, dependencies,
gates, and existing module layout were not changed.

**Resolution paths:**

1. **Rule now (recommended):** approve the opt-in behavior and the file scope
   above, then implement with committed-red tests before production changes.
2. **Research first:** compare long-context task construction and grading
   methods, then return with an amended proposal before implementation.
3. **Spike first:** spend at most one hour on a `spike/` branch validating
   prompt construction and token-count evidence; never merge the spike.

**Acceptance:** Focused tests cover both transport wires, separated two-hop
facts, exact grading, length/cost growth, context refusal, incomplete-run
recommendations, workload identity, resume, and old-record compatibility.
Run `make lint && make test`. A real deep-context model run is a separate
acceptance step whose printed cost and available server window must be checked.

**More information / tags:** capability spec §7.2 `long_ctx_trace`, §13 Q8;
`AGENTS.md` scope discipline (changes touching >5 files).
IMPLEMENTED 2026-09-10. The optional saved plan pins the generator version,
lengths, sampling seed, and 256-token answer budget. Each row retains its
expected answer, observed answer, requested length, calibrated and observed
token counts, context limit, and completion state. Recommendations require
every planned case on both transports, with duplicate/missing/unverified rows
preventing a passing range. Foreign runtimes get answer checks but no context
recommendation because their template and context were not calibrated.

Validation: `make lint && make test` passed (884 unit + 10 integration tests;
19 new trace tests). The initial red commit is `ebbaf03`. A parallel-test
fixture collision was fixed by adding an atomic directory identifier, without
changing the assertions. CLI help and the real four-length dry-run passed;
the latter prints 32 crossings for the active Ornith model and ~334 minutes
including its ordinary throughput suite, using the stated conservative rates.
Zsh completions were regenerated; `shell/_chekov` is ignored by the repository.

Live acceptance passed 2026-09-10 after the user approved the server window.
Ran `target/debug/chekov capability bench --long-ctx-trace 4096,16384,65536,131072 --yes`
against `ornith-1.5-35b-a3b` (weights revision `fbbaed45c2f0`, engine
`0f194b907`, context 262144, seed 42). The complete benchmark, including the
ordinary throughput sweep, exited successfully in about 42 minutes. Its run
is `eval/20260911T001849Z-ornith-1.5-35b-a3b` (the identifier uses UTC).

| Requested prompt length | Observed prompt tokens | Buffered | Streamed |
| --- | --- | --- | --- |
| 4096 | 6472–6492 | 4/4 | 4/4 |
| 16384 | 18735–18757 | 4/4 | 4/4 |
| 65536 | 67875–67896 | 4/4 | 4/4 |
| 131072 | 133425–133461 | 4/4 | 4/4 |

All 32 rows have unique case/transport keys, exact answers, matching calibrated
and response token counts, normal completion, and no truncation. No rows are
missing, duplicated, unavailable, or unverified. The printed recommendation
is `ctx_size = 133717`: the largest observed prompt plus the 256-token answer
reserve, a tested lower bound for this synthetic task set rather than a model
maximum. A read-only comparison of the saved run with itself reproduced the
trace report. The owned server shut down and released its budget; `chekov status`
reports `running no`. The registry context remains 262144.
Proposed 2026-09-10 — status: IMPLEMENTED 2026-09-10; live acceptance passed.

## Harder agentic questions: corpus expansion decision (2026-09-10)

**Question:** Should the default agent benchmark gain a larger set of harder
questions now, accepting that its new results cannot be compared directly with
saved results from the current question set?

**Recommendation:** Make one deliberate corpus update: 39 tool-selection cases
(30 calls and 9 abstentions), 30 paired forced-grammar checks, and 40 instruction
cases. Keep the existing questions and add 23 call cases, 6 abstentions, and 28
instruction cases. This is a proposal; the corpus has not been changed.

The roadmap's current counts are 10 tool cases, 7 grammar checks, and 12
instruction cases, against targets of 30/30/40. Grammar checks are not an
independent question set: the runner repeats each call-expected tool case with
forced output grammar, on the buffered transport only. Abstention cases do not
get that forced pass. Reaching 30 grammar checks while retaining meaningful
abstention coverage therefore requires more than 30 total tool cases. The
recommended 39/30/40 counts supersede the earlier count targets if approved.

**Concrete scope:**

| Addition | Count | Behavior to exercise |
| --- | --- | --- |
| Tool calls | 8 | Choosing among plausible tools with overlapping descriptions |
| Tool calls | 8 | Preserving nested arguments, arrays, types, and optional fields |
| Tool calls | 7 | Exact strings, quotes, escapes, and paths in tool arguments |
| Abstentions | 6 | Three unavailable capabilities and three requests missing required information |
| Instructions | 10 | Combined required and forbidden content |
| Instructions | 10 | Line limits combined with required or forbidden content |
| Instructions | 8 | Fenced Rust output combined with content and line constraints |

Every added case must have one unambiguous expected result and a distinct
failure target, not merely different names or numbers. Use the existing
grader vocabulary. Any case requiring a new checker returns for a separate
scope decision. Retain existing case ids and the six tool-loop scenarios.
Both unconstrained transports remain covered; the forced pass remains buffered.
The single-turn work grows from 51 to 188 crossings per model, plus the existing
variable-turn tool-loop workload. Dry-run estimates must reflect the new counts.

**Compatibility:** Use the existing content hash as the workload identity;
no selector, second loader, or relaxed comparison mask is proposed. The TOML
schema stays at version 0 because its shape is unchanged. Editing its content
changes the hash for every agentic/all run, including runs containing unchanged
tool-loop cases. Old runs remain readable and comparable with other matching
old runs; old/new comparison and resume must refuse. Throughput-only hashes
remain unchanged. Freeze the new content before the measurement campaign and
record its hash with the results.

**Validation:** Follow the existing committed-red protocol, then run
`make lint && make test`. Verify the case counts, all goldens and constraints,
negative answers for each new category, both unconstrained transports, the
call-only forced subset, complete cost estimates, and hash/refusal behavior.
Select three registered models of different capability before measurement;
inspect the printed cost and server availability before running them. Record
per-case and per-category scores, including unavailable grammar results.
Publish any all-pass or all-fail category as non-discriminating, and revisit
its content before claiming the expansion produces useful rankings. This
proposal does not authorize that live campaign or release the separate
compiled-in fixture.

**Scope for approval:** Up to nine existing files, limited to the probe TOML
(`src/core/bench/agentic_v0.toml`), focused inline tests in
`src/core/bench/probeset.rs`, `src/core/bench/probes.rs`,
`src/core/bench/grade.rs`, and `src/commands/capability.rs`, plus `README.md`,
`CHANGELOG.md`, `IDEAS.md`, and `docs/capability-spec.md`. Touch only the sites
needed for the expansion and its validation. Preserve the runner, grader,
persistence layout, dependencies, and gates; no new files are proposed.

**Resolution paths:**

1. **Rule now (recommended):** approve the counts, case categories, one-time
   corpus cutover, and bounded scope of more than five files above, then
   implement and validate the expansion.
2. **Research first:** assess the current per-case results and category gaps
   across three models before approving new questions; use a dry-run to agree
   the measurement cost and server window first.
3. **Spike first:** spend at most one hour on `spike/agentic-probe-expansion`
   drafting a small representative sample with checked goldens; never merge
   the spike. Return with the sample, unresolved ambiguities, and revised scope.

**More information / tags:** capability spec §7.2; the count/comparability
decision in the capability roadmap above; `src/core/bench/probeset.rs:151`
(`content_hash`), `src/core/bench/probes.rs:51` (`suite_prompt_hash`),
`src/commands/capability.rs:1962` (`run_tool_case`), and
`src/core/bench/grade.rs:272` (`check_one`).
Proposed 2026-09-10 — status: IMPLEMENTED 2026-09-10. The user
authorized the recommended counts, corpus cutover, and nine-file scope with
"merge and continue". The corpus now has 39 tool cases (30 calls and 9
abstentions), 30 paired grammar checks, and 40 instruction cases. Its frozen
content hash is `e0d71495afd7`; the original was `6e2669a1c242`. A regression
pins every original question and all six tool-loop scenarios byte-for-byte
below the schema declaration. The format and grader vocabulary are unchanged.

Automated validation: red commit `7699bc0` passed lint and failed ten checks
for the missing cases, unchanged hash, and underestimated multi-model cost.
After implementation, `make lint && make test` passes (894 unit and 10
integration tests). Independent JSON Schema validation confirms all tool
schemas are valid and all 30 golden calls conform to their own schemas.
Good/bad answer fixtures exercise all 23 added calls, all 6 added abstentions,
and all 28 added instruction cases. The single-turn estimate is 188 crossings
per model plus the loop ceiling; a regression also exposed and fixed the
planner counting agentic work only once for a multi-model run.

The CLI dry-run for `--suite agentic --models ornith-1.5-35b-a3b` succeeds and
estimates about 51 minutes on the current configuration. The proposed campaign
uses `qwen3.5-9b`, `ornith-1.5-35b-a3b`, and `gpt-oss-120b`, selected for different
model sizes and families; their actual score spread is still unmeasured.
The three-model dry-run correctly refuses because an existing Ornith server
is running (pid 15573 at validation time). This work did not start or stop it.
The continuation request on 2026-09-10 authorizes the three-model campaign;
it subsequently completed using isolated server state. See the receipt below.

### Three-model campaign preflight (2026-09-11 UTC)

Status: **COMPLETE — 600/600 crossings measured; configured stacks discriminate.**
The heading retains the preflight anchor used by existing links. Fetched origin
and fast-forward checked `develop`: still `a196881`
(PRs #84 and #85 merged), with a clean working tree. Built and used
`target/debug/chekov`; the installed CLI was not used. The corpus SHA-256 is
`e0d71495afd7b79b34a24ea972c020fc9dd4529bba6441c7e931be005dc1c389`.
Its 39 tool cases, 30 forced-grammar checks, 40 instruction cases, and six
tool-loop scenarios remain frozen; no cases or grading changed.

All three registered weight files are present. The live capability scan reports
an Apple M3 Ultra, 262144 MiB RAM, and a 228065 MiB engine-reported GPU budget.
There is 72 GiB free on the checkout volume and 896 GiB on the external model
volume. `capability explain` gives these configured footprints:

| Candidate | Quant / revision prefix | Context | Weights + KV bytes |
| --- | --- | ---: | ---: |
| `qwen3.5-9b` | Q8_0 / `3885219b6810` | 131072 | 11809203424 |
| `ornith-1.5-35b-a3b` | Q8_0 / `fbbaed45c2f0` | 262144 | 40654275840 |
| `gpt-oss-120b` | F16 / `ff1a82da6ad4` | 98304 | 69219388800 |

These are footprint estimates, excluding runtime overhead. Another terminal
initially ran `chekov tune --apply` on
Ornith BF16, then `chekov launch codex` (observed parent pid 91490) started
llama-server pid 91540 on port 8080. That server remained alive after its parent
exited. It belongs to the other session; this campaign did not stop it or send
it inference requests. The three-model dry-run refused the running
`ornith-1.5-35b-a3b-bf16` before producing a plan or wall-clock estimate.
That initial refusal is retained as operational evidence, not a model failure.

The campaign then used the supported `CHEKOV_HOME` override with isolated
config, PID state, and logs under `/tmp/chekov-agentic-campaign-dnx6jfgm`, on
port 18080. The copied configuration differed only in `server.port`; the
model registry was copied byte-for-byte, with symlinks to the same weights,
engine, and `eval` directory. The three-model dry-run passed and estimated
158 minutes; the remaining two-model plan estimated 108 minutes, and the final
GPT-only recovery plan 55 minutes. Resource gates remained enabled. Campaign
servers were started sequentially and only campaign-owned servers were stopped.
The foreign server, pid 91540, remained running afterward. Sampled VM counters
showed no swap-ins, swap-outs, or page-outs; this was still a shared GPU run.

Examined all 43 stored `eval/*/stamp.json` files: none identifies
`agentic-v0:e0d71495afd7` before this campaign. Qwen was interrupted after 192
stored crossings and resumed in the same run. Ornith completed in the remaining
two-model campaign. Its supervisor and the first GPT loader subsequently
disappeared without an exit receipt or GPT stamp; the cause is unknown. A fresh
GPT-only run completed successfully. Completed measurements were reused, and
no failed case was rerun to select a better score.

All three final runs contain exactly 200 unique expected crossings: 78 tool,
30 forced grammar, 80 instruction, and 12 loop rows. There are no missing,
duplicate, partial, ungraded, or unavailable rows. The engine is `0f194b907`,
machine stamp `c057455fb3a1`, prompt-set hash `ac1955773bbf`, seed 42, and request
temperature 0. No production code, cases, grading, or model configuration changed.

| Candidate | Run ID | Tools | Forced / paired unconstrained | Instruction strict / loose | Loops |
| --- | --- | ---: | ---: | ---: | ---: |
| Qwen | `20260911T034515Z-qwen3.5-9b` | 36/39 | 29/30 / 27/30 | 9/40 / 9/40 | 6/6 |
| Ornith | `20260911T041044Z-ornith-1.5-35b-a3b` | 35/39 | 29/30 / 27/30 | 29/40 / 30/40 | 6/6 |
| GPT-OSS | `20260911T043659Z-gpt-oss-120b` | 35/39 | 27/30 / 27/30 | 39/40 / 39/40 | 5/6 |

Tools, instructions, and loop pass/fail agree on both transports for every
case; table values apply separately to each transport. Forced grammar is
buffered only and uses the recorded `deepseek` reasoning extraction. The
strict totals are 131, 169, and 185 of 200 crossings, respectively; these totals
weight paired transports twice and should not replace the individual axes.

Normal `capability compare` refuses every pair because contexts differ. The
preserved descriptive comparisons explicitly use `--cross-runtime --cross-flags`.
Besides context and quantization, Ornith enables MTP draft length 1, and GPT
uses engine-default reasoning formatting while Qwen/Ornith request `none`.
These are comparisons of registered model stacks, not a controlled model-only
ranking. Shared GPU activity also prevents an isolated speed comparison. One
seed and paired transports do not establish statistical robustness.

| Category (per transport) | Qwen | Ornith | GPT-OSS |
| --- | ---: | ---: | ---: |
| Legacy tool calls | 7/7 | 6/7 | 6/7 |
| Legacy abstentions | 3/3 | 2/3 | 3/3 |
| Overlapping tools | 8/8 | 7/8 | 7/8 |
| Nested / typed arguments | 6/8 | 7/8 | 8/8 |
| Exact strings / escaping | 6/7 | 7/7 | 6/7 |
| Unavailable capabilities | 3/3 | 3/3 | 2/3 |
| Missing required information | 3/3 | 3/3 | 3/3 |
| Legacy instructions | 6/12 | 11/12 | 12/12 |
| Required / forbidden content | 1/10 | 4/10 | 10/10 |
| Line / content constraints | 2/10 | 7/10 | 9/10 |
| Fenced Rust / combined constraints | 0/8 | 7/8 | 8/8 |

**Discrimination and saturation.** Of 115 distinct axis/case pairs, 45 have
mixed pass/fail outcomes and 70 pass on all three stacks; none fails on all
three. Counting transports yields 86 mixed and 114 all-pass crossings. The
expansion contributes 34 mixed pairs among its 80 added axis/case pairs,
including 25 of 28 added instruction cases. Instruction scores distinguish
all three stacks; tools are near ceiling and tie Ornith/GPT in aggregate while
their failures differ. Forced grammar is also near ceiling, with 26/30 common
passes. Missing-information abstentions saturate at 3/3, and five of six loops
pass everywhere. Qwen has a model-specific floor on the eight added fenced
Rust cases; there is no shared all-model floor. GPT has a ceiling on required /
forbidden content and fenced Rust, and near ceiling on instructions overall.

**Per-case failures.** Tool failures on both transports are Qwen `te-021`,
`te-022`, `te-032`; Ornith `te-003`, `te-010`, `te-015`, `te-022`; and GPT
`te-003`, `te-017`, `te-032`, `te-036`. Recurring failures include reading
instead of editing (`te-003`, Ornith/GPT), preserving a JSON null cursor
(`te-022`, Qwen/Ornith), and exact JSON text plus a newline (`te-032`, Qwen/GPT).
Ornith fabricates `grep` for the deletion abstention (`te-010`); GPT calls
`stat_file` when asked to change permissions with inspection-only tools
(`te-036`). Qwen emits no call for typed zero/false/empty values (`te-021`).
Ornith's edit preview (`te-015`) and GPT's definition lookup (`te-017`) have
argument mismatches. The retained rows do not contain the actual arguments.

Forced failures are Qwen `gg-te-013` (`walk_tree` instead of `list_dir`),
Ornith `gg-te-003` (read instead of edit), and GPT `gg-te-003`, `gg-te-022`
(null-cursor arguments), `gg-te-029` (forced reply not JSON for exact newline /
tab content). The aggregate grammar gains of 2, 2, and 0 cases hide regressions:
Qwen regresses on `te-013`; GPT regresses on `te-022` and `te-029` while fixing
`te-017` and `te-032`. GPT alone exhausts eight turns with eight calls on
`tl-005`, the legacy off-by-one repair, on both transports. Reached-loop turn
min/median/max are Qwen 3/4/6, Ornith 4/6/7, GPT 4/5/6 buffered and 4/5/5 streamed.

All 31 Qwen instruction failures per transport have zero visible-answer
characters and nonzero thinking characters. The fixed instruction budget is
512 tokens (`src/core/bench/probes.rs:144`); engine-log tasks 9749 and 10263
confirm 512-token generations for buffered `if-002` and `if-003`. Budget
starvation is a supported explanation for those rows, and a hypothesis for
the other empty failures, not proof of an inability to write Rust. Ornith's
11 instruction failures are `if-009`, `if-014`, `if-018`–`if-022`, `if-024`,
`if-026`, `if-030`, and `if-033`; only `if-026` passes loose grading, and only
`if-019` / `if-033` have empty visible answers. Several failures retain forbidden
substrings (`failed`, `acct-482`, `override`, `error:`). GPT's sole instruction
failure is `if-028`, missing the exact substring `dry-run`. It has no observed
zero-answer crossing with nonzero thinking.

**Measurement blind spot.** Qwen `if-006` passes on both transports despite
zero answer characters: its checks only forbid `process` and cap line count,
which an empty answer satisfies. Qwen `te-008` similarly passes the no-tool
contract without a visible answer to the requested definition. These are
retained as original passes, not rescored. The JSONL records grades, character
counts, timing, and loop summaries, but not full response bodies or turn
transcripts. Exact mismatched arguments, the malformed forced response, and
the sequence of GPT's repeated calls therefore cannot be reconstructed here.

**Evidence and validation.** The portable
[raw evidence archive](docs/agentic-campaign-20260911.tar.gz) contains all three
`eval/<run>/stamp.json` and `results.jsonl` pairs, the 200-row three-model outcome
matrix in `logs/agentic-campaign-20260911-analysis.json`, all pairwise comparison
and refusal logs, engine output, preflight/isolation/recovery receipts, and
validation logs. Its `manifest.json` gives SHA-256 hashes for every member.
Original gitignored artifacts remain on the measurement machine. `make lint &&
make test` passes: 897 unit and 10 integration tests. Earlier sandbox failures
denied localhost binding; unrestricted runs pass. No product fix was made, so
there is no new committed-red cycle in this documentation/evidence change.

**Recommended next item / TODO (proposed, not a ruling):** resolve visible-answer
and generation-budget observability before expanding the corpus again. A
successful check can currently conceal no answer, while a low score can reflect
the fixed thinking budget. The resolution paths are: rule now that answer-required
instruction checks must reject empty output; research first (recommended) to
reproduce empty passes, retain termination/response evidence, and distinguish
budget exhaustion from parsing or model behavior; or a one-session `spike/`
branch to trial diagnostics, never merged. File the resulting scope/decision
here before implementation and use committed-red TDD for fixes. Preserve this
campaign unchanged and give any changed grading/budget a distinct comparison
identity. The separate compiled-in fixture remains deferred.

## Stop reason on graded agentic rows (2026-09-13)

First slice of the observability follow-up recommended by the campaign above.
Every graded agentic row — `tool_emit`, `instruction`, `grammar_gap`, fixture
probes, and `tool_loop` — records the crossing's own `stop_reason` as
`reply.stop_reason`, read from the same body the grader reads; the loop captures
the final reply's reason as it lands. Rows written before the field load as
`None`, and a failed crossing carries no stamp. This retains termination
evidence so a zero-answer pass or a low score can later be attributed to budget
exhaustion (`max_tokens`) rather than a withheld answer (`end_turn`). No grade,
case, checker, or comparison identity changes; the 2026-09-11 campaign stays
untouched.

**Reports (second slice, 2026-09-13).** The run report appends
`(stop: <reason>)` to every stamped agentic FAIL line and lists strict
`instruction` passes that spent thinking but returned no visible answer as
`instruction PASS <id>  empty visible answer`; the comparison's disagreement
lines carry the same suffix on a failing side. Only `instruction` is screened
for empty passes — a tool call is a legitimately textless pass, and telling an
abstention case from a call case needs the case's `expect`, which rows do not
record. Grades, counts, and comparison identity are unchanged.

**Live acceptance 2026-09-13 (MDT).** `bench --suite agentic --models
ornith-1.5-9b` (run `20260914T030415Z-ornith-1.5-9b`, 200 rows, ~8 min): every
row carries `reply.stop_reason` — `tool_use` 68, `end_turn` 116, `max_tokens`
16. The stamp already answers the campaign's question for this model: of its
six buffered instruction failures, four (`if-003`, `if-019`, `if-033`, `if-037`)
stopped on `max_tokens`, and the two with zero visible answer characters are
both budget-starved rather than withheld. Two instruction passes (`if-013`,
`if-021`) and two tool passes (`te-034`, `te-035`) also hit `max_tokens` with
text already emitted. No strict pass on this model had an empty answer. Not a
campaign measurement: `cargo test` builds ran on the same machine during the
crossings, so its timings are not evidence.

Still open from the TODO: the ruling on rejecting empty visible answers for
answer-required instruction checks (now with the count and cause on hand), and
reproducing the campaign's empty passes on the three campaign models.
Proposed 2026-09-13 — status: IMPLEMENTED in PR #86 (rows: red `ddf8f44`, green
`783a138`; reports: red `771d3c0`, green `d434aac`).

**Research for the ruling (2026-09-13, MDT).** The campaign archive shows the
instruction gap is mostly a harness artifact: instruction probes cap
`max_tokens` at 512 and tool probes at 256, thinking counts against that cap,
and Qwen3.5-9B's 32 empty buffered instruction rows all carry 1448–2267
thinking characters, right at the ceiling, while its 8 answered rows all
thought less. A scratch binary with instruction at 4096 and tools at 1024 (not
committed; run `20260914T033943Z-qwen3.5-9b`, buffered half complete) moved
Qwen's strict instruction score from 9/40 to 28/40 and its empty answers from
32 to 11; the 11 still empty burned the whole 4096 on thinking and stay real
failures. Of the 29 answered rows only 20 fit under 2048 tokens and 27 under
3072, so the cap must be 4096. Tool probes never touched 1024 (longest reply
1450 chars). Cost: a runaway case burns the full cap at ~30 tok/s, so the
buffered half took ~50 min against ~8 for Ornith-9B. The streamed half was
lost to a manual `chekov stop` at 22:33 and is not needed for the decision;
its 15 measured cases agree with buffered. Vacuous passes are rare: `if-006`
is the only instruction case whose checks an empty reply satisfies, and
`te-008` the only silent abstention in the campaign. Sources consulted agree
on treating `max_tokens` with an empty answer as failure and on giving
reasoning models room above the visible answer.

**Ruling APPROVED 2026-09-13 (human approval in chat: "I approve"),** on this
recommendation, in priority order:
1. **Budget.** Instruction probes at 4096 and tool probes at 1024
   `max_tokens`, folded into the agentic hash so a run under the ruling never
   compares with, or resumes, a run before it. Then one three-model campaign
   under the new identity.
2. **Grading.** An empty visible answer fails every instruction case, strict
   and loose (`empty visible answer`), and an abstention case that emits no
   visible text fails (`abstained without answering`). A grader version rides
   in the same hash. Both land in one committed-red cycle; the 2026-09-11
   campaign stays untouched.

**Re-measurement under the ruling (2026-09-14 MDT, 22:42–01:03).** One
`bench --suite agentic --models qwen3.5-9b,ornith-1.5-35b-a3b,gpt-oss-120b`
run from the ruling's binary, agentic prompt-set hash `ab4cd9b077d2`; runs
`20260914T044245Z-qwen3.5-9b`, `20260914T062919Z-ornith-1.5-35b-a3b`,
`20260914T064636Z-gpt-oss-120b`, 600 rows, every row stamped. `capability
compare` refuses the old Qwen run against the new one naming
`prompt_set_hash` (`ac1955773bbf` vs `ab4cd9b077d2`), as ruled. Buffered,
old → new:

| Axis | Qwen 3.5 9B | Ornith 1.5 35B A3B | GPT-OSS 120B |
| --- | --- | --- | --- |
| instruction strict | 9/40 → 27/40 | 29/40 → 32/40 | 39/40 → 36/40 |
| tool_emit | 36/39 → 37/39 | 35/39 → 36/39 | 35/39 → 35/39 |
| grammar_gap | 29/30 → 29/30 | 29/30 → 29/30 | 27/30 → 28/30 |
| tool_loop | 6/6 → 6/6 | 6/6 → 6/6 | 5/6 → 3/5 |

Pass/fail agreed across transports on every case but one (GPT `tl-002`:
buffered exhausted its 8 turns, streamed reached the goal). Qwen recovered 19
instruction cases and lost `if-006`, its one vacuous pass, to `empty visible
answer`; 24 of its rows still stop on `max_tokens` and every one of those is a
runaway thinker graded as a failure with the reason attached. Ornith recovered
`if-019`, `if-021`, `if-033` and `te-015`. GPT lost `if-003`, `if-021`, and
`if-022`, all `end_turn` with text (`if-021` wrote 9,864 answer characters and
tripped `not_contains:bypass`): the wider cap lets a verbose model talk its way
into a forbidden substring, and at temperature 1.0 single crossings move —
neither is a grading artifact. GPT `tl-004` buffered is unavailable: the engine
answered HTTP 500 ("output does not match the expected peg-native format"),
an engine-side harmony parse, not a model failure. No strict pass on any model
has an empty answer. Of 114 axis/case pairs graded on all three, 32
distinguish the configured stacks and 81 pass everywhere (campaign: 45 of 115
and 70; the old figures reproduce exactly from the archive). The instruction
axis carries 21 of the 32, down from 32 of 45. The same caveat holds as before:
contexts, quantization, MTP and reasoning settings differ, so this ranks
configured stacks, not models. Wall clock: Qwen ~1 h 47 min (runaway cases burn
4096 tokens per door), Ornith and GPT ~17 min each. Raw evidence: the three
`stamp.json` / `results.jsonl` pairs, the run log, and a SHA-256 manifest are
in [docs/agentic-campaign-20260914.tar.gz](docs/agentic-campaign-20260914.tar.gz).
Not a controlled timing measurement: no other load ran, but the bench binary
was a debug build.

## Harder tool-loop scenarios: loop corpus expansion decision (2026-09-14)

**Question:** Should the six `tool_loop` cases grow now, accepting one more
comparability break, before any new model is measured on the tool-use axis?

**Why now.** Under the 2026-09-13 ruling the instruction axis discriminates
(21 of the 32 separating pairs) but the loop axis is blind: Qwen and Ornith
score 6/6, five of six loops passed on every campaign stack, and GPT-OSS's
only loop failure mode is turn exhaustion. The report already prints
"saturated: rank across candidates" on that line. The approved 2026-09-09
measurement of new tool-use candidates (Muse Spark 1.3, Nemotron 3.5
Lightning) was justified as "hold the tool-use verdict for `tool_loop`", so
running it on a saturated axis would have to be redone.

**Recommendation:** grow to twelve loop cases, keeping `tl-001`–`tl-006`
byte-for-byte, with each addition exercising one anti-saturation device the
current set lacks. Same five tools, same eight-turn budget, each new case
solvable in six turns or fewer by a competent agent, each with one
unambiguous terminal state and a distinct failure target.

| id | device | shape | what a failing model does |
| --- | --- | --- | --- |
| `tl-007` | find before edit, with a decoy hit | The prompt names a symbol. `grep` hits two files: the call site in `src/main.rs` and the definition in `src/limits.rs`. Goal: definition edited, `src/main.rs` untouched. | Edits the first grep hit. |
| `tl-008` | the `edit_file` uniqueness contract | The wrong literal occurs twice in the target file (a doc comment and the code). `edit_file` answers "old text occurs 2 times; make it unique". Goal: the code line changed, the comment line still present. | Retries the same `old`, or edits the comment. |
| `tl-009` | the tool result contradicts the prompt | The prompt blames `src/parse.rs`; `run_tests` fails naming `src/limits.rs` and the expected value. Goal: `src/limits.rs` edited, `src/parse.rs` untouched, tests green. | Edits the file the prompt named and stops. |
| `tl-010` | already done | The requested change is already in place. Goal: `unchanged`, final reply mentions `already`. | Makes a no-op or spurious edit, or reports a change it did not make. |
| `tl-011` | three-file chain | `src/api.rs` documents a limit "per `crate::policy`"; `src/policy.rs` says the value comes from `src/limits.rs`; the wrong constant is in `src/limits.rs`. Goal: `src/limits.rs` edited, the other two untouched. | Edits `policy.rs` or `api.rs`, or stops after one hop. |
| `tl-012` | two edits behind one test gate | Two constants in `src/config.rs` are wrong; `run_tests` fails naming both until both are fixed. Goal: both present. | Fixes one, sees "ok"-shaped progress in its own head, stops. |

**Schema.** `tl-007`–`tl-011` fit `Goal::Edited { file, contains_any,
untouched, tests_fail }` and `Goal::Unchanged { reply_mentions }` as they
stand. `tl-012` needs one addition: `contains_all: Vec<String>` beside
`contains_any` on `Edited` (every string must be present; `run_tests` reports
`ok` only when all are). Load-time validation extends to it: none of the
strings may be present before the first turn, and a goal may carry
`contains_any` or `contains_all`, not both. TOML schema version stays 0 if the
field is optional with a default; bump it only if the reviewer wants old
readers to refuse the file.

**Compatibility.** The TOML text is what `content_hash` hashes, so this
changes the agentic prompt-set hash again; old agentic runs refuse to compare
or resume by construction, throughput hashes are unchanged, and the
2026-09-11 and 2026-09-14 campaigns stay readable. Dry-run estimates pick up
the new loop count automatically.

**Cost.** Cases and validation ~1 session; one three-model campaign under the
new identity (~2.5 h, Qwen's runaway thinkers dominate). Then the Muse /
Nemotron measurement runs once, on an axis that can separate.

**Not proposed:** per-case turn budgets, scoring the path, a real
filesystem, or non-Rust repositories (loop design §11 keeps them out).

Proposed 2026-09-14 — status: **APPROVED 2026-09-14 (human approval in chat:
"I approve")** as proposed: six cases `tl-007`–`tl-012`, the optional
`contains_all` goal field, schema version unchanged, then one three-model
campaign under the new identity before the Muse / Nemotron measurement.

**Measured 2026-09-14 (MDT, 10:03–12:06).** One `bench --suite agentic
--models qwen3.5-9b,ornith-1.5-35b-a3b,gpt-oss-120b` run on the twelve-loop
set, agentic prompt-set hash `57c7585512ec`; runs `20260914T160304Z-qwen3.5-9b`,
`20260914T173233Z-ornith-1.5-35b-a3b`, `20260914T174834Z-gpt-oss-120b`, 636
rows, every row stamped. Raw evidence with a SHA-256 manifest:
[docs/agentic-campaign-20260914-loops.tar.gz](docs/agentic-campaign-20260914-loops.tar.gz).

| Axis (buffered) | Qwen 3.5 9B | Ornith 1.5 35B A3B | GPT-OSS 120B |
| --- | --- | --- | --- |
| tool_loop | 12/12 | 10/12 | 7/11 (+1 unavailable) |
| instruction strict | 27/40 | 32/40 | 36/40 |
| tool_emit | 37/39 | 36/39 | 35/39 |
| grammar_gap | 29/30 | 29/30 | 28/30 |

The loop axis discriminates now: five of twelve loop cases separate the
stacks (`tl-002`, `tl-005`, `tl-007`, `tl-010`, `tl-011`), against two of six
before. Three of the added devices did the separating: the decoy grep hit
(`tl-007`: Ornith and GPT both edited the call site and stopped with the
constant unchanged), the already-done change (`tl-010`: GPT called tools for
all eight turns instead of reporting), and the three-file chain (`tl-011`:
Ornith's sixth turn hit the loop's 512-token cap mid-call and ended as
`'edit_file' called without path`). The uniqueness contract, the
contradicting test, and the two-edit gate (`tl-008`, `tl-009`, `tl-012`) passed
on every stack. Qwen, the weakest instruction follower, is the strongest
looper: every case closed, median four turns. Pass/fail agreed across
transports on every case measured both ways. Of 120 axis/case pairs graded on
all three stacks, 35 distinguish and 84 pass everywhere (ruling run: 32 of
114 and 81); the loop axis contributes 5 of the 35.

Every single-turn axis reproduced the ruling run's verdicts exactly — zero
flips across 188 cases per model — so the seeded sampling is reproducible
run to run and the loop numbers stand on the same footing. GPT `tl-004`
buffered was unavailable again with the same engine-side HTTP 500 (the
Harmony `peg-native` parse, see the ruling entry); with seeded sampling it
reproduces, so a retry would not have recovered it. Wall clock: Qwen 89 min,
Ornith 16 min, GPT 18 min.

**Follow-up (proposed, not a ruling):** `loop_probe` still sends
`max_tokens: 512` per turn, the cap the 2026-09-13 ruling raised everywhere
else; Ornith's `tl-011` failure is that cap cutting a tool call in half. Raise
the loop turn cap in the same way (one constant in the hash) before the Muse /
Nemotron measurement, so a loop failure means the model, not the budget.

**Ruling APPROVED 2026-09-14 (human approval in chat: "approve"):** raise
`loop_probe` to 4096 tokens per turn, the instruction cap, as a constant that
rides in the agentic identity hash; then one three-model run under the new
identity before the Muse / Nemotron measurement.

**Measured 2026-09-14 (MDT, 12:56–14:48).** Same three models, agentic
prompt-set hash `7882af966fae`; runs `20260914T185606Z-qwen3.5-9b`,
`20260914T201544Z-ornith-1.5-35b-a3b`, `20260914T203110Z-gpt-oss-120b`, 636
rows, every row stamped. Raw evidence with a SHA-256 manifest:
[docs/agentic-campaign-20260914-loopcap.tar.gz](docs/agentic-campaign-20260914-loopcap.tar.gz).
Against the twelve-loop run, exactly one verdict changed across all 636
rows: Ornith `tl-011` now closes on both doors (11/12 loops, from 10/12), and
no loop turn on any model stops on `max_tokens` any more. Every other row —
188 single-turn cases and the other eleven loops, per model — reproduced its
previous verdict, so the cap change did precisely what it was ruled to do and
nothing else. Loops: Qwen 12/12, Ornith 11/12, GPT-OSS 7/11 (+1 unavailable,
the same `tl-004` engine-side parse error). Four of twelve loop cases
separate the stacks (`tl-002`, `tl-005`, `tl-007`, `tl-010`); of 120 axis/case
pairs, 34 distinguish and 85 pass everywhere. Ornith's one loop failure and
both of GPT's non-exhaustion failures are the decoy grep hit (`tl-007`), now
the sharpest loop case in the set. Wall clock: Qwen 79 min, Ornith 16 min,
GPT 17 min. The Muse / Nemotron measurement can run on this identity.

## A forcing mechanism for `grammar_gap` on thinking-prefill templates (2026-08-28)
`response_format` json_schema is refused (HTTP 400, "Failed to initialize
samplers") by this engine for `ornith-1.5-35b-a3b`, so the §7.2 grammar_gap
axis reports N/A on it. Root cause, verified in source and reproduced
live: on `/v1/chat/completions` llama.cpp builds a grammar whose root is
`"<|im_start|>assistant\n" space response-format`, then prefills the FULL
generation prompt — which this template ends with `<|im_start|>assistant\n<think>\n`
— through that grammar sampler. The root cannot accept the `<think>\n` the
template itself emitted, so sampler init throws before a single token is
generated. `/completion` (no generation prompt, no prefill) accepts every
schema shape including oneOf+const, so the schema converter is not at fault.

Candidate mechanisms, and where each stands:

(a) **raw GBNF via the `grammar` field — REJECTED, do not build this.** It does
    return 200 and did produce a correct forced call for te-002 (raw grammars
    are USER-type and skip the prefill), which makes it look attractive. It is
    strictly worse than the current N/A. The reply comes back as
    `<think>\n{...}`: the template's `<think>` is prompt-emitted, so no grammar
    rule can consume it, and because the grammar forbids `</think>` the span
    never closes. `strip_thinking` refuses an unterminated span BY DESIGN, so
    grading sees no text and every forced case becomes a SILENT failure —
    trading loud engine errors for quiet fabricated model failures, the exact
    trade this axis exists to prevent. Making it correct would mean hardcoding
    each model family's reasoning-tag convention into the grammar root, and
    would forbid reasoning in the forced arm while the unconstrained arm
    reasons freely — a confound injected into the very number designed to
    detect self-deception.

(b) **per-request `"reasoning_format":"deepseek"` — VALIDATED 2026-08-29, SHIPPED.**
    Returns 200 on the same schema that 400s, because that flag gates whether
    the `<think>` alternative enters the grammar (`chat.cpp:1187`
    `extract_reasoning`). Live on engine 0f194b907 with `max_tokens=200`:
    `content` = `{"name": "get_weather", "arguments": {"location": "Paris"}}`,
    the reasoning in `reasoning_content`, `finish_reason: stop`. The earlier
    doubt was only the 30-token budget. Built as `runner::FORCED_REASONING_FORMAT`
    on the forced wire ONLY (the unconstrained and streamed wires are
    byte-identical to before); the run head records it and the `grammar_gap`
    line prints `forced pass ran with reasoning extracted (deepseek)`, so the
    one extra difference from the unconstrained arm is named, not hidden.
    Also established 2026-08-29: the human's cherry-picked fix (0f194b907,
    `chat-auto-parser-generator.cpp`) is correct for AUTOPARSER templates
    (MiniMax-M2) but ornith's template (`<tool_call>` + `<think>` +
    `<|im_start|>`) is routed to the SPECIALIZED handler at `chat.cpp:1166-1300`
    (hardcoded `GEN_PREFIX`), whose grammar root the server logged as
    `root ::= "<|im_start|>assistant\n" space response-format` — no `<think>`
    alternative. The upstream re-port belongs at `chat.cpp:1233-1239`; chekov
    does not wait for it.

(c) **Patch llama.cpp upstream — worth a PR, but sequence nothing behind it.**
    The narrow fix is in `chat.cpp`'s specialized handlers: build the prefix
    from `data.generation_prompt` rather than the hardcoded `GEN_PREFIX`, or
    admit the `<think>` alternative whenever `supports_reasoning` regardless of
    `extract_reasoning`. chekov must never depend on it: chekov tracks
    tip-of-master with no pin, users run whatever they built, and an upstream
    merge does not retroactively repair anyone's binary.

Open question for the human: §7.5 says an N/A axis withholds the composite,
but §7.5's weight table gives `grammar_gap` ZERO weight — it is a diagnostic
control, not a scored axis. Withholding on it would make the composite
permanently unobtainable on any llama.cpp build with a thinking template.
Decide before a composite is implemented.

Note a false-pass hazard for whoever builds this: an EMPTY schema, and
`response_format: {"type":"json_object"}`, both return 200 with UNCONSTRAINED
prose — no grammar is attached at all. A preflight probing with `{}` would
conclude structured output works and then fabricate passes, the mirror image
of the failures this N/A change removed. Probe with a non-empty schema only.
Proposed 2026-08-28 — status: RESOLVED (mechanism (b) shipped 2026-08-29, see
above); the §7.5 composite question stays open until a composite exists

## Streaming probes for bench (2026-08-28)
Spec §7.1 asked for probes over the STREAMING seam as well ("what makes
streaming-only defects reachable — interleaved parallel tool-call deltas, an
upstream error frame swallowed into a fake `end_turn`, an unterminated
`<think>` eating the turn"). Only the non-streaming half was built, and the
first agentic run found exactly that class of bug: the streaming translator
stripped thinking spans while the non-streaming one did not, so the two halves
of the same translator disagreed about what the agent receives. Claude Code
streams; the bench did not — so the bench was grading a path the agent never
takes. Fixed for thinking, but the asymmetry class remains until probes cross
`stream_translator()` the way `serve::relay` does.
SHIPPED 2026-08-28: `runner::cross_streaming` puts `stream: true` on the
Anthropic request, pumps the SSE body through a fresh `stream_translator()`
exactly as `serve::relay` does, and reassembles the agent-side events into the
message an SDK client holds at `message_stop`, so the same graders read it.
Every unconstrained agentic case now crosses BOTH doors (no flag — Claude
Code's door is not optional); rows carry `transport`; the report prints
`asymmetry <suite> <case>: buffered PASS, streamed FAIL — <reason>` for every
case that disagrees with itself. An `error` frame is `BenchStreamFailed` —
recorded unavailable, never a forged `end_turn`. Still buffered by design: the
throughput sweep (its numbers are upstream timings either way) and the
grammar-forced pass (its axis is the grammar gap). Still out of reach, as §7.1
states: the socket — `serve.rs`'s HTTP/1.1 framing and chunked encoding.
Proposed 2026-08-28 — status: SHIPPED

## `EndpointDown` claims "not answering" for a request that WAS answered (2026-08-28)
A 400 refusal renders as "endpoint ... is not answering ... restart with
`chekov restart`". The endpoint answered — it refused — and restarting cannot
help when the request itself is unacceptable. Surfaced by the grammar_gap N/A
message, whose remediation advice is actively misleading. Wants a distinct
variant for "the upstream refused this request" carrying the server's own
explanation, now that `hub::post_json` preserves it.
SHIPPED 2026-08-29 as `ChekovError::UpstreamRefused { url, status, reason }`
via one classifier, `serve::answered`, used by `hub::post_json` and
`get_bearer`: 2xx is the body, anything else is a refusal carrying the
status and the server's own words, and the message says the server is up and
the request is what to fix (`chekov show`, logs/llama-server.log).
`EndpointDown` keeps its meaning — connect/send/read failures, readiness
timeouts, an unparseable `/props`. The bench's forced-pass latch now fires
on `UpstreamRefused` only; before, a dead socket mid-run would have been
written off as an engine limitation.
Proposed 2026-08-28 — status: SHIPPED

## The non-streaming translator drops `reasoning_content` (2026-08-28)
`to_anthropic_response` reads `message.content` and `message.tool_calls` only.
A model served with `--reasoning-format auto|deepseek` puts its reasoning in
`message.reasoning_content`, which the streaming path turns into a `thinking`
block and the non-streaming path silently discards (§C.2: nothing degrades
silently). Not currently reachable — every registry entry uses
`--reasoning-format none` — which is why it is filed rather than fixed.
Became reachable 2026-08-29 when the bench's forced pass started asking for
`reasoning_format: deepseek` per request. FIXED the same day: a non-empty
`reasoning_content` is the first content block, `{type: thinking, thinking,
signature: ""}` — the block `ClaudeStream::on_thinking` opens — ahead of the
text and `tool_use` blocks; a test holds the two paths to the same block
sequence. Graders read text blocks only, so bench verdicts are unchanged; the
stored artifact simply stops losing the reasoning.
Proposed 2026-08-28 — status: FIXED

## Bench GLM-5.3-Flash — blocked on upstream llama.cpp (2026-08-28)
`unsloth/GLM-5.3-Flash-GGUF` (arch `glm5_next`, released 2026-08-26) needs
llama.cpp PR #27754 (https://github.com/ggml-org/llama.cpp/pull/27754), which
is not on `master` as of 2026-08-28 (`d7bd3bfca`): `llama-arch.cpp` there has
no `glm5*` entry. `chekov update --engine` tracks master only, so the engine
cannot reach it, and building from a PR branch would stamp every run with a
non-master commit that no later run could compare against. Human's call
2026-08-28: skip until the PR merges. When it does: `chekov update --engine`,
then `chekov pull unsloth/GLM-5.3-Flash-GGUF:UD-Q3_K_XL --model-loc
/Volumes/jane/models` (137.4 GiB; UD-Q4_K_XL is 186 GiB and tight against the
222.7 GiB budget before KV), then `chekov capability bench --models
glm-5.3-flash`. Qwen3.8-Flash-Next (`qwen4exp`) IS on master and is being
benched in the same pass.
Status check 2026-08-30: PR #27754 is still an open DRAFT (unslothai branch,
updated 2026-08-30, `mergeable_state: blocked`) — still not on master. The PR
body confirms the model carries an MTP block at layer index 45 (relevant to the
MTP-awareness idea below). Sizing note: 1-bit dynamic is ~93-100 GB, so even
when the PR merges this model is Studio-class only — it cannot fit a 48 GB
M4 Max under any published quant.
2026-09-06: the Aug 24–31 upstream weekly report still lists `glm5next` among
the OPEN pull requests — blocked as before; nothing to do here yet.
Proposed 2026-08-28 — status: BLOCKED (upstream)

## A cell's second character ignores the overhead's provenance (2026-08-28)
`frontier::Cell::inputs()` reports `#` (measured) whenever KV is measured,
regardless of `overhead_bytes.provenance` — and `build_frontier` gives every
cell a flat predicted 3 GiB overhead. So a cell can print "measured" while one
of its three summands is a constant guess. Defensible as shipped (KV is the
term that varies with context and dominates the total; a second character that
was always `·` would carry no information), but it is a real gap between the
glyph and the arithmetic. The SVG's per-cell tooltip prints each part's own
provenance, which is the honest version; the glyph is the lossy summary.
Noticed while building `--svg`; not changed there, because it would alter
shipped terminal output and its tests.
RESOLVED 2026-08-29 (human's call: fix the legend, keep the glyph). The
second character now says what it encodes — `#  kv measured   ·  kv
predicted` — and the legend line ends with what it does not cover, derived
from the cells: `overhead is a flat predicted 3.0 GiB in every cell`. A third
"mixed" mark was rejected: with the overhead predicted everywhere it would
replace every `#` and carry no information, whereas the legend line adds the
fact the reader lacked. `Cell::inputs` is `kv_inputs` now, reading KV alone.
Proposed 2026-08-28 — status: RESOLVED

## Throughput dots in the SVG (2026-08-28)
Spec §5 wants the SVG to carry measured throughput as filled dots with p10-p90
whiskers and predicted throughput as hollow dots with a ±15% range. Not built:
`Frontier` carries no speed at all, because the "`--metric tok-s` grid upgrades
from predicted to measured" line is still deferred (see below). Blocked on the
same work — once stored bench medians reach the frontier model, both the ASCII
grid and the SVG gain the layer together, from one source.
UNBLOCKED 2026-08-28: `Cell.speed` now carries the measured median with p10-p90,
so the filled-dot layer has its source. The hollow predicted dots do not — no
predicted tok/s reaches the frontier model, and the ±15% band is an unvalidated
prior — so the layer should ship measured-only first.
SHIPPED 2026-08-29, measured-only: a "decode tok/s (measured)" panel under the
grid on the grid's own ctx columns — a filled dot at the median with a p10–p90
whisker and the number + row name beside it (identity never rides on colour);
nothing for a cell without a run, and no panel at all without a measurement.
A label that would run across the next dot flips to its dot's left; same-side
labels a line apart drop a line. The SVG legend states that predicted
throughput is not drawn and why. Verified in Chrome (`getBBox`: no overlaps,
no overflow). The hollow predicted dots stay out until a validated predictor
exists.
Proposed 2026-08-28 — status: SHIPPED (measured); predicted dots OPEN

## Feed measured bench medians into `capability graph` (2026-08-27)
Slice 5's spec line "the `--metric tok-s` grid upgrades from predicted to
measured" is deliberately deferred from the harness change: wiring stored
`logs/bench/` medians into the slice-2 grid touches every graph rendering
surface and needs a staleness rule (a measurement from an older
engine.build_commit must not silently pose as current). Do it as its own
change once a few real runs exist.
SHIPPED 2026-08-28 as `capability graph --metric tok-s`. Two decisions
recorded here because they deviate from or sharpen the spec: (1) the band
digit uses FIXED edges (5/10/15/20/30/40/60/80 tok/s), not the §5.2 "deciles
of decode rate" — deciles of the peer set move a cell's digit when a
different model is benched, the objection §7.5 already adopted for
composites; (2) the headline per run is the decode median at the DEEPEST
summarisable depth, named in the legend — the closest to an agent loop with a
full context, where a shallow probe flatters every model. A run applies to a
cell only on an exact model+quant+ctx+machine match; the latest of several is
shown and the choice is a footnote; rule 8's stale footer names both builds.
Proposed 2026-08-27 — status: SHIPPED

## Tool-parser gate: report, do not refuse (2026-08-27)
Slice 4 of the capability spec makes "falls through to llama.cpp's generic PEG
autoparser" a HARD REFUSAL under `--role agent`. Replaying the real cascade
(`llama.cpp/common/chat.cpp` ~3430-3552) against live templates shows that gate
would reject `unsloth/MiniMax-M2.7-GGUF` — the author's own daily driver, marked
`hermes_ok = true` in `models.toml`. Its 6594-char template carries
`<minimax:tool_call>` and `<invoke name=` but not the `]<]minimax[>[` namespace
token that llama.cpp's only MiniMax arm (M3) requires, so it falls through and
still works. Fallthrough means "no dedicated parser", not "cannot call tools".
`core::toolparser` therefore classifies and reports; it does not refuse.
RESOLVED 2026-08-27 by the human: `recommend --role agent` DOWNRANKS a
fallthrough candidate with a printed note rather than rejecting it. Implemented.
Proposed 2026-08-27 — status: RESOLVED

## Downloader status bar for `pull` (2026-08-29)
`pull` is silent for the minutes-to-hours a 40 GB shard takes, so a working
download and a hung one look identical. Proposal: a per-shard progress line on
stderr — bytes / total, MiB/s, ETA, shard N of M — plain text when stderr is not
a TTY, and nothing new on stdout so `pull` stays scriptable. Sibling of the
partial-shard resume ("Replace hf-hub…", 2026-08-25): both need the same
per-shard bookkeeping, so whichever lands first should build it for the other.
Rationale: today's 397B pull is 5 × ~39 GB with no feedback at all.
SHIPPED 2026-08-29 together with the resume, as predicted: `core/progress.rs`
holds `Progress` (a pure `line`), `CountingReader` (≤ 1 tick/second, always at
EOF) and `Sink { Tty, Plain }`, and `hub::download_shard` builds one per shard
and hands its resumed offset to the `Range` request.
Proposed 2026-08-29 — status: SHIPPED 2026-08-29

## Portability sweep: any Apple Silicon Mac, not this Mac Studio (2026-08-29)
chekov says it is for Apple Silicon, but it has only ever run on one M3 Ultra
with 256 GB. A first count (2026-08-29) finds no machine constant in production
code — `M3 Ultra` / `228065` appear only in a doc-comment example and tests, and
`187000` only in `config.toml`, the `config.example.toml` comments and README
prose — so the audit surface is: the `iogpu.wired_limit_mb` handling (12 sites in
5 files: the 75% default, the sysctl read, the "required vs actual" check) on
macOS versions and chips where the default differs; the sysctl/ioreg keys and
the `llama-server --list-devices` line shape on M1–M4 and 8–512 GB parts;
`machine_id` and the stamp's machine fields; tests that assert this machine's
numbers; and every doc example written about this desk. Proposal: audit each,
move anything machine-specific behind config or detection, and rewrite the
examples so they are illustrations rather than this machine's values.
Acceptance: on a 16 GB M1, `chekov capability`, `doctor`, `recommend` and `graph`
give honest output — every model reads "exceeds", nothing crashes, no number is
invented; the docs' examples do not assume this machine; and CI or a test pins
that no machine constant is hard-coded outside config. Rationale: the tool is
"for Apple Silicon", not "for this desk".
Part 1 SHIPPED 2026-08-30 — the one constant that bit: the compiled-in
`wired_limit_mb = 187000` refused every model on any Mac under ~250 GB. The
floor is now opt-in (`Option`, default absent) and `run` judges the model's own
footprint against the live budget through one shared `core::footprint`;
`setup`/`status` say what is checked; a test pins that no production path in
config/checks/machine/footprint/run/setup/status/pull decides with this desk's
numbers (doc comments may still illustrate with them). Still open: the
`--list-devices` line shape and sysctl keys on M1–M4 parts other than this one
(only verifiable on those machines), and the spec's worked examples.
Proposed 2026-08-29 — status: PART 1 SHIPPED; hardware sweep OPEN

## `compare` shows the cross-file arms and the lift side by side (2026-08-30)
The codebase section of `capability compare` pairs rows by task id and groups them
by tier label, so a `cross_file_first` task's two arms (`<id>` and `<id>+extra`)
land in one group of twelve and the per-model `context lift` — the number B1
exists to produce — is not compared at all. On the first B1 pair (pushkin,
`20260830T070907Z-ornith-1.5-397b` vs `20260830T072140Z-qwen3.8-flash-next`) the
lifts were `+0.17/−0.01/−0.02/−0.17/−0.13` vs `+0.33/+0.31/+0.37/−0.17/+0.24`, the
clearest separation in the run, and the section printed a single blended
`cross_file_first` line. Proposal: split the group into `cross_file_first` and
`cross_file_first+extra` (pair by full task id, as the report does) and add a
`context lift` row comparing the two models' lifts per tier with the same paired
sign test over tasks present in both arms of both runs. Rationale: the tier that
separates models should be the tier `compare` reads best.
SHIPPED 2026-08-30 as proposed: groups are (tier, arm) — `cross_file_first` and
`cross_file_first+extra` — and a `context lift` group compares the per-task
lifts (extra − no_extra) on tiers 1-5, compile and test under the paired sign
test; a task both runs touched but one measured on one arm only is dropped
from the lift with its own drop line.
Proposed 2026-08-30 — status: SHIPPED

## Bench a foreign runtime: MTPLX and MLX servers as first-class candidates (2026-08-30)
MTPLX (mtplx.com, Apache-2.0, MLX-native) decodes Qwen 3.5/3.6/3.8 — and
community MTP-grafted builds of our own bench subjects, e.g.
`philipjohnbasile/ornith-ai-Ornith-1.5-35B-A3B-V2-MTPLX` and
`wang-yang/Ornith-1.0-35B-MTPLX` (measured 1.53x on an M3 Max) — around
1.4-2.2x faster than autoregressive on the same Apple hardware, by running the
model's own multi-token-prediction head as a drafter with exact rejection
sampling (claim: output distribution unchanged). One independent write-up also
measured the same 27B model at 10.5 tok/s under llama.cpp vs 18.3 under MLX —
runtime choice alone was +74%. chekov already benches through its own
translator against a running server (`StepAction::UseRunning`), so most of the
plumbing exists; the gaps are (a) codebase mode rides llama.cpp's `/infill`,
so a foreign OpenAI/Anthropic-compatible server needs a chat-completions FIM
fallback for the codebase corpus, and (b) the stamp assumes a llama.cpp engine
commit — it needs a runtime name+version field so `compare` refuses across
runtimes by a named field instead of comparing incomparables. Payoff: chekov
becomes the referee that can measure MTPLX's speed claim AND test its
exactness claim empirically — same corpus, same HEAD, tiers 1-7 llama.cpp vs
MTPLX, with the B2 exec tiers checking that the "identical" fills still
compile and pass. Nobody else's harness can do that today.
SHIPPED 2026-08-31: `capability bench NAME --runtime <name>@<version>
[--upstream <url>]` makes a foreign OpenAI-compatible server a
`UseRunning`-only bench subject — chekov never launches one, and refuses
(`RuntimeNeedsRunningServer`) before any measurement if the subject isn't
already serving. Readiness is a plain `GET /v1/models` with served ids
printed, never asserted; unmanaged launch flags stamp as fixed sentinels
(`ctx`/`n_parallel` `0`, six flag fields `"unmanaged"`) instead of invented
ones. `Stamp` gains `runtime` (serde-default `llama.cpp`; every run already
on disk reads unaffected), and `BenchStampMismatch` is now engine-neutral.
Codebase mode gained a chat-completions FIM fallback for runtimes with no
`/infill` (the report names the transport), and `capability compare
--cross-runtime` permits exactly the runtime/build/unmanaged/prompt-hash
fields to differ, behind a loud banner ("this measures the runtimes, not the
model."). Cut from this pass: `--runtime` together with `--judge` is refused
by the existing memory-budget gate — a
foreign server chekov did not launch never comes down, so the judge has
nowhere to load beside it — and live verification against a real MLX/MTPLX
server is approval-gated and still owed; the plumbing ships unit-tested
against fakes on the existing `HttpClient` seam.
SHIPPED 2026-08-31 (timing design): foreign runs no longer need llama.cpp's
`timings` object at all — a foreign run is stream-timed by chekov's own wall
clock over the SSE response instead (OpenAI `usage` token counts plus two
measured windows, request-to-first-frame and first-frame-to-stream-end;
decode divides by n-1 tokens; `cache_n` recorded `0`), and it is honest
about its own limit: client-side timestamps include wire and translator
overhead, and the first-frame mark only approximates end-of-prefill because
these servers stream tokens as they are generated. That two-window split is
the honesty limit of a buffered SSE read. A reply chekov cannot derive a
timing from (no `usage`, fewer than 2 completion tokens, a zero-length
window) still fails loudly per probe, naming the runtime and the exact
reason. `Stamp` gains `timing_source` (`server-reported` default; every run
already on disk reads unaffected), the report prints a `timing source:` line
only when it isn't the default, and `--cross-runtime` permits it to differ.
SHIPPED 2026-09-01: agentic and fixture suites now ride the same clock.
Fixture crosses via `pass.clock.cross` exactly like throughput — llama.cpp's
buffered door unchanged, a foreign run timed over the stream. Agentic keeps
both doors' real transports (comparing them is the suite's point), but a
foreign run's buffered door — where an MLX-style server answers chat fine
yet reports no `timings` object — now rides a new untimed crossing
(`runner::cross_untimed`) instead of failing `BenchNoTimings`; its row
records the empty measure (`codebase::run::empty_measure()`) beside a real
grade, never an invented zero. The streamed door, and the grammar-forced
probe on a foreign run, still derive real timings via `cross_stream_timed`.
llama.cpp's agentic/fixture rows are byte-for-byte unchanged.
LIVE VERIFICATION DONE 2026-08-31 against mlx-lm 0.31.3 serving
ornith-ai/Ornith-1.5-35B-A3B-MLX (bf16) on this machine: stream-timed
throughput measured 55.5/56.0/54.4 tok/s decode at depths 1024/4096/16384
(prefill 1.9-2.7K), the codebase suite graded all 24 tasks over the chat
arm (`fim transport: chat`; in_file exact 0.17, cross_file 0.33 -> 0.50
with the extra file), and `--cross-runtime` against the 2026-08-28
llama.cpp Q8_0 run printed the banner and read llama.cpp faster at every
depth (78.6/77.7/68.1) — a quant-confounded number (Q8_0 vs bf16), noted
on the record, not a runtime verdict. Two interop findings:
(a) SHIPPED 2026-08-31 (this change): chekov sent its registry name as the
OpenAI `model` id; llama-server ignores it but mlx-lm routes on it and
404s trying to download that name. `--served-model <id>` now names which
served id is the subject explicitly; absent it, a single served id is
used automatically, and a server listing zero or several without the flag
refuses (`RuntimeServedModelRequired`) rather than guessing — the
registry name still names the run directory, the stamp and the report,
never the request wire on a foreign run. (b) DOCUMENTED 2026-08-31: a
thinking-default model burns the gold-bounded fill budget on reasoning
and every chat fill fails loudly as "chat fill has no text content"
(honest N/A, recorded in run 20260831T201602Z); serving with
`enable_thinking: false` fixed it live, and the README's foreign-runtime
section now says so — still owed: growing the chat-FIM arm its own
thinking-budget strategy instead of relying on the operator to disable it.
2026-09-06: the live MTPLX referee run is still owed and still approval-gated
(installing MTPLX is a machine-level dependency). New data point for when it
runs: MTPLX 2.10.x publishes 64.3 tok/s on Qwen3.8-27B at 3K context on an
M5 Max, 18.4 at 147K, and a Claude Code follow-up turn with 165,165 of
165,502 tokens served from cache — the deep-context regime chekov's sweep
does not reach today. `--runtime` needs nothing new for it.
Proposed 2026-08-30 — status: SHIPPED; foreign-timing measurement SHIPPED
2026-08-31; live MLX verification DONE 2026-08-31; finding (a) SHIPPED and
finding (b) documented 2026-08-31; foreign agentic/fixture timing SHIPPED
2026-09-01 — remaining: a thinking-budget strategy for the chat-FIM arm
(finding (b))

## MTP-head awareness: `explain` reports it, bench measures it (2026-08-30)
Qwen 3.5+/3.8, Gemma 4 and GLM-5.x ship native MTP heads in their weights
(GLM-5.3-Flash's llama.cpp PR names its MTP block at layer 45), and llama.cpp
currently drops them on the floor — the entire MTPLX niche exists because
runtimes ignore a ~2x decode speedup already sitting in the artifact.
`capability explain` reads GGUF headers; teach it to detect and report "carries
a native MTP head (unused by this engine)" so the fit/recommend story names the
latent speed. When llama.cpp lands an MTP decode path, bench grows a
speculative row (accept rate by depth, measured speedup vs the AR baseline);
until then it is an honest "engine has no MTP path" skip in the existing
skip-with-reason machinery, never a zero.
SHIPPED 2026-09-01 (spec `docs/superpowers/specs/2026-09-01-tune-spec-stage-design.md`):
the engine gained `--spec-type draft-mtp` (runs on the main weights' nextn
tensors; no draft file) in August, so the "engine has no MTP path" skip never
had to exist — the measurement landed as `chekov tune`'s first stage instead
of a bench row. Spike on ornith-1.5-35b-a3b Q8_0 @ ctx 262144: baseline 71
tok/s; draft length 3 (engine default) 61–63; length 2 70–77; length 1 85–89
at 58–81% acceptance — a 3B-active MoE trunk is cheap, so only a one-token
draft pays. `explain` now points at the stage; the stamp names `spec_type`
and `spec_draft_n_max`; compare refuses across them. A bench-side accept-rate
row (from `/metrics` `spec_decode_*`) is a possible later slice.
ACCEPT-RATE SHIPPED 2026-09-05, and not from `/metrics`: llama-server puts
`draft_n`/`draft_n_accepted` on every response's `timings` object when it
drafted, so bench reads them where it already reads the four rates — no
extra flag, nothing polled. Summed per depth into the row, printed as
`accept N% (M drafted)` on the depth line and as a sweep total on the
`speculative:` header; zero rows and pre-field rows print nothing.
Proposed 2026-08-30 — status: SHIPPED 2026-09-01

## `chekov tune`: per-machine launch-flag autotune with an honest verdict (2026-08-30)
Sweep `n_batch`/`n_ubatch`/KV cache types/`flash_attn` against a fixed probe on
THIS machine, save the winning argv per model with the measured before/after,
and print "defaults won" when nothing beats them — the honest-verdict pattern
`mtplx tune` uses (it keeps the AR baseline and refuses to save a depth that
did not win). Optionally record thermal pressure at run start/end in the stamp
so a throttled run explains its own variance — MTPLX pins fans for clean
timing; chekov can at least say when the clock was dirty. Fits the §12
portability-sweep idea: tuned-per-machine beats tuned-for-this-desk.
SHIPPED 2026-08-30 as proposed: `chekov tune [NAME] [--dry-run] [--yes]
[--apply] [--stages fa,kv,batch,ubatch]` runs a four-stage descent from the
model's own flags (`fa`/`kv` judged on decode, `batch`/`ubatch` judged on
prefill), each candidate winning its stage only against `[bench]
significance_pct`, degenerate trials excluded from every comparison and never
saved, and the honest **`defaults won`** verdict — with the threshold it was
reached under — printed and stamped on every run with nothing to beat the
baseline. Every run writes a JSON record under `tune/<utc>-<model>.json`.
Thermal pressure is read via `pmset -g therm`'s `CPU_Speed_Limit` before and
after every probe (no root needed); the true pressure API is a C notification
this crate cannot link under `#![forbid(unsafe_code)]`, so a nominal reading
is honestly `None`, not a false "fine". `--apply` writes the winner into the
model's `extra_flags` only after printing the exact diff and a confirm —
`defaults.flags` is never touched. Left out, as scoped: `--exhaustive` (the
full 64-launch grid), any axis beyond the four (`--threads`,
`--n-gpu-layers`, `--kv-unified`, `--fit`), and looping several models in one
invocation.
Proposed 2026-08-30 — status: SHIPPED

## New benchable models on a 48 GB Mac (survey 2026-08-30)
Qwen3.8-27B dense (released ~2026-08; qwen3_5-family arch, llama.cpp support
live, Ollama ships `qwen3.8:27b`): Q4 is ~16-18 GiB — fits the 48 GB M4 Max
beside `ornith-1.5-35b-a3b`, and is the natural head-to-head since Ornith 1.5
builds on the Qwen 3.5 base line. A Qwen3.8-9B exists (community quants on HF)
for the small lane vs `ornith-1.5-9b`. NOT benchable on 48 GB:
Qwen3.8-Flash-Next (125B-A6B + 51B n-gram; 1-bit GGUF ~73 GB, ~83 GB resident
even with the n-gram table on SSD — 96 GB+ machines) and GLM-5.3-Flash
(320B-A18B; 1-bit ~93-100 GB, and still blocked upstream — see the BLOCKED
entry above). Both remain Studio-class candidates only.
HEAD-TO-HEAD DONE 2026-09-01 (runs 20260901T070018Z-ornith-1.5-35b-a3b vs
20260901T064618Z-qwen3.8-27b — same engine 0f194b907, ctx 131072, flags,
seed, corpus, exec tiers, judge gpt-oss-20b at 100% swap consistency;
ornith re-benched at qwen's ctx because compare correctly refuses a ctx
mismatch): decode 80.7/79.8/74.3 vs 23.3/23.2/22.3 tok/s at depths
1024/4096/16384 — ornith 3.5x faster everywhere, as MoE 3B-active vs 27B
dense predicts. Quality tied: tool_emit 8/10 both, grammar_gap 6/7 both,
instruction 12/12 vs 11/12 (one separating case, if-009, both doors), and
NO codebase metric separates at p < 5% under the paired sign test (qwen
ahead on in_file exact 0.42 vs 0.25, ornith ahead on cross-file
parse/symbols — none significant at n = 6-12). Verdict: ornith-1.5-35b-a3b
stays the daily driver; qwen3.8-27b buys no measured quality for 3.5x
slower decode. The 9B small-lane face-off remains unrun.
9B FACE-OFF DONE 2026-09-02 — first, a correction to the survey: Qwen
never released a Qwen3.8-9B. The repos by that name are `empero-ai`'s
community distill of Qwen3.8-27B into the official `Qwen/Qwen3.5-9B` base
(~350K downloads, so it is what the small lane actually runs), and the
official small model is Qwen3.5-9B itself. All three were benched, Q8_0
each, ctx 131072, engine 0f194b907, same corpus (chekov @ 00874d1), exec
tiers, judge gpt-oss-20b (runs 20260902T214520Z-ornith-1.5-9b,
20260902T215041Z-qwen3.5-9b, 20260902T215800Z-qwen3.8-9b-distill):
SPEED is a three-way tie — 55.9 / 55.3 / 55.6 tok/s decode at depth
1024, 53.3 / 53.0 / 52.4 at 16384, no pair separates (dense 9B is dense
9B). AGENTIC splits by axis: tool_emit 8/10 (ornith: te-003 read_file for
edit_file, te-010 a fabricated call) vs 10/10 (qwen3.5) vs 9/10
(distill); grammar_gap 6/7 vs 7/7 vs 7/7; instruction 11/12 (ornith) vs
6/12 (qwen3.5 — five `fenced_rust_only` failures, one `contains`) vs 9/12
(distill), both doors agreeing on every case. CODEBASE: ornith-1.5-9b
leads in_file exact 0.42 vs 0.17 vs 0.17 and edit_sim 0.71 vs 0.61 vs
0.56, ties or trails on function_body and cross-file symbols — nothing
significant at n = 6-12 under the paired sign test, and the judge column
is n/a (0-1 eligible crossings per run). Verdict: no 9B wins outright.
ornith-1.5-9b is the small-lane pick for instruction-bound coding work
(constraints, in-file fills); qwen3.5-9b is the pick when tool selection
is the whole job; the distill sits between them on both axes and buys
nothing the official base does not. Note that ornith-1.5-9b's in_file
exact (0.42) matches qwen3.8-27b's and beats ornith-1.5-35b-a3b's 0.25
from the 2026-09-01 pair — a dense 9B fills a masked line as well as the
big models on this corpus; what the 35B buys is the 3.5x decode speed and
the agentic ceiling (12/12 instruction, 8/10 tool_emit at 80 tok/s).
Proposed 2026-08-30 — status: DONE (both face-offs measured)

## The MTP draft head on real code: +21-31% decode, identical output — and tune's prefill guard says no (2026-09-03)
Measured 2026-09-03 on `ornith-1.5-35b-a3b` Q8_0 @ ctx 262144, chekov's own
codebase at 6a6458a as the corpus, exec tiers, judge gpt-oss-20b. First the
full five-stage `chekov tune --apply`: **defaults won** — `kv f16`, every
`batch` and every `ubatch` candidate is "no significant difference", `fa off`
is the named skip under q8_0 KV, and the only candidate that moved anything,
`spec mtp:1`, was rejected by the stage's own rule: decode 75.4 vs 69.3 but
prefill 128 vs 147 ("faster on decode but slower on prefill — incumbent
kept"). Then `--spec-type draft-mtp --spec-draft-n-max 1` hand-applied to
`extra_flags` and the same bench again (runs
`20260903T031503Z-ornith-1.5-35b-a3b` untuned vs
`20260903T044019Z-ornith-1.5-35b-a3b` drafted; `compare --cross-runtime` to
mask the two flag fields the stamp now carries): decode 86.2 / 88.6 / 76.4
vs 66.8 / 67.5 / 63.2 tok/s at depths 1024 / 4096 / 16384 (+29 / +31 /
+21%, every depth significant); prefill 145 / 144 / 113 vs 147 / 137 / 125
(a cost only at 16K, and a wash at 4K); and EVERY quality metric identical —
tool_emit 8/10, grammar_gap 6/7, instruction 11/12 with no disagreements on
any case, and all 30 codebase crossings tie on every tier including compile
and test. The head is a free 21-31% on the daily driver's actual work.
Two follow-ups this exposes. (a) `tune`'s spec stage judges "not slower on
prefill" at one depth (4096) and rejects a candidate the workload wants —
the guard is right in spirit (a second graph does cost prefill) but wrong in
threshold: proposal, judge the spec stage's prefill on a tolerance
(`[tune] prefill_tolerance_pct`, default maybe 15) rather than on
significance alone, or on a combined tokens-per-second-at-workload figure —
a design choice to make deliberately, not a default to flip. (b)
`compare --cross-runtime` is the only way to compare two llama.cpp runs that
differ on a launch flag, and its banner then reads "cross-runtime
comparison: llama.cpp vs llama.cpp … this measures the runtimes, not the
model", which is the wrong sentence for a flag experiment: proposal, a
`--cross-flags` mask (the six flag fields plus the two speculative ones,
nothing else) with a banner that names the flags and says "this measures
the launch flags, not the model". The flags were LEFT APPLIED on
`ornith-1.5-35b-a3b` after this measurement.
(b) SHIPPED 2026-09-03 as `capability compare --cross-flags`: exactly the
eight flag fields masked, the banner names the differing flags, a differing
runtime still refuses, and the two masks compose.
(a) SHIPPED 2026-09-03 as `[tune] guard_tolerance_pct` (default 15, `0` =
the strict rule), the human's choice of the tolerance over a workload
figure: the verdict line names the trade and the tolerance, the `defaults
won` line names both thresholds, and the record stamps the tolerance it
judged under. The workload-figure alternative stays unbuilt.
Live check of (a) 2026-09-05 (`tune ornith-1.5-35b-a3b --stages spec`,
untuned incumbent, record `tune/20260906T022935Z-ornith-1.5-35b-a3b.json`):
`mtp:1` decode 74.7 vs 67.9, prefill 126 vs 150 — "faster on decode but
prefill -16% is beyond the 15% guard — incumbent kept"; defaults won. The
same trade measured −13% on 2026-09-01 and 2026-09-03 and −16% here, so on
this machine it sits ON the default tolerance, not inside it: three runs,
one point either side. The knob behaved exactly as designed and the phrase
says why; whether the default should be 20 rather than 15 is a per-machine
call (`[tune] guard_tolerance_pct` in config.toml), not a reason to move
the shipped default after one candidate. The draft flags remain applied on
the daily driver from the 2026-09-03 codebase measurement, which is the
stronger evidence.
Proposed 2026-09-03 — status: MEASURED; (a) and (b) SHIPPED

## tune's fa stage cannot measure `fa off` under quantized KV, and a dead candidate reads as a timeout (2026-09-01)
Two full live tunes (ornith-1.5-35b-a3b 2026-08-30, qwen3.8-27b 2026-09-01,
records in `tune/`) both marked the `fa off` trial degenerate "not ready
after 600 polls". A `--stages fa` reproduction with the server log captured
shows the real cause: llama.cpp exits at load with `quantized V cache
requires flash_attn to be enabled` — the fa-off candidate keeps the
incumbent's `--cache-type-v q8_0`, an engine-invalid combination, so with
this machine's q8_0-KV defaults the fa stage can never measure `fa off` at
all. Three fixes, in order of value: (a) tune should refuse-or-rewrite the
combination — either skip the fa-off candidate with a named reason when the
incumbent KV is quantized (the mirror of the spec's kv-skip rule) or trial
it with f16 KV, a design choice to make deliberately; (b) the trial's
server DIED at load yet tune burned the full 600-poll budget and reported a
timeout — the runner's own creed says "a server that dies while loading
must fail as 'died' (go read the log), never as a timeout", so the
pid-watch is not seeing the daemonized child's death; (c) every teardown in
both tunes — including the already-dead fa-off pid — logged "ignored
SIGTERM for 20s — escalating to SIGKILL", so `stop_pid` is likely
signalling a stale or wrong pid (probably the same daemonization gap as
(b)). The degenerate rule kept both tunes honest, so nothing recorded is
wrong — but ~5 min per tune is wasted and the reported reason mislabels a
knowable refusal.

**(a) SHIPPED 2026-08-31** — `fa_skip` (src/commands/tune.rs, mirroring
`kv_skip`) skips the `fa off` candidate before any spawn when the
incumbent's V-cache (`-ctv`/`--cache-type-v`) is quantized, naming the
incumbent's actual spelling in the reason. Not (b) as speculated: never
trials with f16 KV substituted, since that would silently measure a
different configuration than the one the descent is actually running.

**(b) and (c) SHIPPED 2026-08-31** — root cause was neither pid-watch nor
`stop_pid` picking the wrong pid: `spawn_daemon_with_env` spawns
llama-server as a direct child and never reaps it, so a dead child sits as
a ZOMBIE, and a zombie passes the signal-0 probe (`process_alive`)
forever — for `chekov run` this is correct (chekov exits and launchd
adopts the child), but bench/tune stay resident and never see the death.
`server::child_alive` reaps via `waitpid(WNOHANG)` before answering (with
a signal-0 fallback for a pid this process did not spawn), and now backs
both `bench::runner::wait_ready`'s readiness poll and `server::stop_pid`'s
grace-period poll — a candidate that dies at load now reports "died" in
seconds instead of after 600 polls, and a cooperative teardown reports
`Terminated` promptly instead of burning the full 20s grace on a corpse.
Proposed 2026-09-01 — status: (a)/(b)/(c) SHIPPED 2026-08-31

## tune's spec stage skips every model without an MTP head — the engine has five drafter-free n-gram types (2026-09-06)
The stage's first skip is "no head in the GGUF" (`nextn_predict_layers 0`),
which is most of this registry — `gpt-oss-20b`/`-120b`, `minimax-m2.7`,
`gemma-3-12b-it`. But the pinned engine's `--spec-type` (checked 2026-09-06
on `0f194b907`) accepts `none, draft-simple, draft-eagle3, draft-mtp,
draft-dflash, draft-dspark, ngram-simple, ngram-map-k, ngram-map-k4v,
ngram-mod, ngram-cache` as a comma-separated list tried in order, and the
`ngram-*` types draft from the prompt's own repetition with no draft file
and no head — the rewrite-a-file turn of an agent session is exactly that
shape (MTPLX's "cache-copy drafting" is the same trick, +19% on that turn in
its 2.10.0 note). Each has its own knobs (`--spec-ngram-mod-n-min/-max/
-n-match`, `--spec-ngram-simple-size-n/-m/-min-hits`). Proposal: `[tune]
spec_drafts` accepts `ngram:<type>` beside `off` and `mtp:<n>`; the
head-absent skip narrows to the `mtp:` candidates and says so ("no head —
MTP candidates skipped; n-gram trialed"); the engine's `--help` gate checks
the type name in the list, not just the flag; the stamp's `spec_type`
already carries whatever the flag says, so `compare` refuses by name
unchanged. Two honesty limits to build in: n-gram acceptance is
workload-bound and tune's 4096-token probe is prose-shaped, so a win or a
loss there says little about code — the 2026-09-03-style decode-on-the-
codebase check is the confirming measurement; and chaining
(`draft-mtp,ngram-mod`) is a second candidate grammar, not this one. Not a
quality question: every draft is verified by the target, so greedy output
is unchanged.
Design APPROVED 2026-09-09 (`docs/superpowers/specs/2026-09-09-ngram-spec-design.md`,
binding; §13 amended after a six-agent seam map and critique). SHIPPED
2026-09-09: `ngram:<type>` as a third spelling; per-candidate skips (the head
gates `mtp:` only, the engine's type list by whole token, `ngram-mod` and
`ngram-cache` by name because their draft table outlives the request and the
repeated probe would replay the first reply); the acceptance clause on every
drafting trial; the closing caution that the three lookup types cannot draft
on the probe. Live acceptance owed to the stopped-driver window.
Proposed 2026-09-06 — status: SHIPPED 2026-09-09

## tune judges at 4096 tokens; the workload lives at 50–165K (2026-09-06)
`[tune] depth = 4096` is the only depth any stage measures, and a flag's
cost is not flat in depth: MTPLX's own release note has Qwen3.8-27B decoding
3.5x slower at 147K than at 3K on one Mac; a 2026-08-31 A100 measurement of
a llama.cpp KV fork had `q8_0`/`q4_0` KV at 64K under 30% of f16's decode
(quantized-KV attention leaves the fast kernels at depth) — Metal is
unmeasured, which is what tune is for. Claude Code's first turn on a real
repo is 100–165K tokens (MTPLX logs 165,165 of 165,502 cached on a
follow-up). So a `kv q8_0` or `mtp:1` verdict at 4K can invert where the
user actually sits, and every guard debate of 2026-09-01/03/05 was a
one-depth debate. Proposal: `[tune] depths = [4096, 65536]` (default stays
`[4096]` — nothing changes until a machine opts in); a candidate wins only
if it wins at the shallow depth AND is not `Slower` beyond
`guard_tolerance_pct` at the deep one; the verdict names both depths; the
record stamps the list. The cost is honest and large — 64K of prefill on
`ornith-1.5-35b-a3b` is ~7.5 min per probe at 145 tok/s, ~40 min per
candidate at the default five repetitions — so the plan line and the
`--dry-run` estimate carry it; whether the deep depth gets the full
repetition count or a single confirming probe is a design choice to make
deliberately when this is built, not a default to flip. The config-only
half needs no code: `[bench] depths` gaining 65536/131072 puts a measured
point where the agent regime is, and `--metric tok-s` on the frontier then
shows it; the same wall-clock honesty applies.
IMPLEMENTED 2026-09-10: opt-in `[tune] depths` overrides the legacy single
`depth`; the default remains 4096. Every depth gets the full
`[bench] repetitions` count and its own warmup drop. A shallow win must also keep both
deep metrics within the configured regression guard at every deeper depth.
The plan estimates the whole sweep, and records/reports carry per-depth
measurements. A failed depth prevents a win and preserves earlier evidence;
old single-depth records still load. Live acceptance is recorded below.
Cache correction IMPLEMENTED 2026-09-10: the live check recorded on
`feat/tune-depths` (`43f56f3`, run `20260910T050933Z-ornith-1.5-35b-a3b`)
found that repeated prompts reused their prefix cache: retained samples
evaluated only four fresh tokens, so their prefill medians could not validate
the full-prompt guard. Tuning now sends `cache_prompt: false` after translation
on every repetition at every depth; the plan names the full-prefill cost.
Each depth records the maximum server-reported `cache_n`, including warmup;
old records load with that count unknown. Any reported cache reuse prevents
a verdict, retains the cached measurement and earlier depths, and names the
reason. Ordinary capability benchmarks keep their existing cache policy.
Regression tests cover the request wire, retained cache evidence, and old
records.
Live acceptance PASSED 2026-09-10 on `c9ff5d7`: Ornith Q8_0 revision
`fbbaed45c2f0`, engine `0f194b907`, ctx 262144. The 4K smoke record is
`tune/20260910T225924Z-ornith-1.5-35b-a3b.json`; the `[4096, 65536]`
comparison is `tune/20260910T230114Z-ornith-1.5-35b-a3b.json` (727 seconds).
Both used five repetitions per depth, retaining four after each warmup drop.
All 25 probes had `cache_n = 0`; server logs confirmed every repetition
evaluated the full 4,127 or 65,567 prompt tokens. Decode medians at 4K/64K
were 81.4/57.8 tok/s for the existing `mtp:1` baseline and 70.0/54.8 for
drafting off; prefill medians were 2164/972 and 1862/1003 tok/s respectively.
At the existing 5% comparison threshold and 20% guard, off lost on shallow
decode; both deep guards passed. The recorded "defaults won" verdict matched
an independent audit of medians, percentile ranges, and guard calculations
against the server logs. This run did not exercise a deep veto or find new
winning flags. Config was restored byte-for-byte, the registry was unchanged,
and the server returned to its original stopped state. Local evidence,
including logs, the verification JSON, and restoration hashes, is under
`reports/fresh-prefill-acceptance-20260910T225909Z/` (ignored).
Proposed 2026-09-06 — status: IMPLEMENTED 2026-09-10 (approved 2026-09-09)

## Reasoning effort is a launch flag nobody stamps, and a cost nobody measures (2026-09-06)
Three findings point one way. (1) chekov's own 2026-08-31 foreign run: a
thinking-default model burned the whole bounded fill budget on reasoning
and every chat fill was an honest N/A until the operator disabled thinking
server-side — "growing the chat-FIM arm its own thinking-budget strategy"
is still owed above. (2) Every Qwen3.8-27B guide says it "wildly overthinks
by default" and to run it at low/medium effort; a 45-configuration RTX 5090
sweep (2026-08-29) found reasoning effort moved time-to-visible-answer ~10x
— more than any server flag it tried. (3) The pinned engine (checked
2026-09-06) has `--reasoning [on|off|auto]`, `--reasoning-effort LEVEL`
(`minimal` … `max`), `--reasoning-budget N` (`0` = end thinking at once)
and `--reasoning-budget-message`; all four are launch flags a registry
entry can carry in `extra_flags` today, with zero chekov code. What is
missing is honesty about them. Proposal, two halves: (a) the bench `Stamp`
reads `reasoning_effort` and `reasoning_budget` off the launch argv through
the same `stamp::LaunchFlags` reader the six flags use (serde default
`engine-default`; every stored run loads unchanged), so `compare` refuses
two runs that differ only in how much the model was allowed to think —
today that difference is invisible and would read as a model verdict — and
`--cross-flags` masks them like the other eight; (b) bench records, per
probe, reasoning tokens against answer tokens (the translator already
splits `reasoning_content` into a `thinking` block — count it, never
re-parse) and, on the streamed door, time-to-first-visible-text; the report
prints a `think share` column beside the pass counts. Why chekov
specifically: an agent backend that is right but 10x slower to its first
visible token loses to one that is slightly wrong, and no chekov number
sees that today. A per-entry registry key is NOT proposed — `extra_flags`
already is one, and a second spelling of the same flag is the
knob-for-a-value-that-never-varied mistake. The `think_leak` probe (§13 Q5)
is a different question — where the thoughts land, not how many.
Design APPROVED 2026-09-09 (`docs/superpowers/specs/2026-09-09-reasoning-stamp-design.md`,
binding): reasoning flags join the stamp and both compare masks; every row's
measure gains thinking and answer CHARACTER counts read off the upstream body
— the only place a `--reasoning-format none` run's `<think>` span still
exists — because llama-server reports no reasoning token count anywhere
(checked `tools/server/` at 0f194b907); the report prints one `thinking` line
of per-suite median shares. §11 amended the same day after a seven-agent seam
map and critique: seven flags, not four; stored runs hydrate their flags from
their own argv; unclosed spans count to the end; a tag table of the six
families llama.cpp leaves inline, Kimi named as the gap it cannot close.
SHIPPED 2026-09-09 (both halves): fifteen flag-sourced stamp fields,
load-time hydration, `thinking_chars` / `answer_chars` on every row, the
`thinking` report line. IMPLEMENTED 2026-09-10: `compare` shows both runs'
median recorded thinking share per suite and transport, using the same cases
with character counts in both runs. Measured/shared pair counts expose missing
measurements; an entirely unmeasured comparison is `N/A`, not zero thinking.
Still owed: time-to-first-visible-text (the mark belongs in `hub.rs`'s
streamed-read loop, which is agent-frozen).
Proposed 2026-09-06 — status: SHIPPED 2026-09-09

## New tool-use lane candidates for the agentic bench (survey 2026-09-06)
Two Aug-2026 30B-class releases aim at the axis where our benched models
sit near-saturated on single-turn `tool_emit`. Meta's Muse line — the ~30B
dense checkpoint one guide calls Muse Glimmer and a release timeline lists
as Muse Spark 1.3 (pin the HF repo before registering): ~29.6B incl. a 1.8B
vision encoder, Apache-2.0, 131K ctx, self-reported MCP Atlas 75.5 against
Qwen3.6-27B's 62.5, llama.cpp-supported, under 20 GB at a K-quant. And
NVIDIA Nemotron 3.5 Lightning (30B-A3B MoE, 2026-08-11): tau-bench 0.640
measured independently by Thoughtworks, a built-in speculative head that
gave 1.46–1.96x there — hybrid Mamba-2, so llama.cpp support and a GGUF
must be verified first, and `explain` should say whether the head appears
as `nextn_predict_layers`. Both fit this Mac and the 48 GB seat. Proposal:
register, `explain`, bench `all` + `--codebase` + `--judge gpt-oss-20b`
against `ornith-1.5-35b-a3b` at ctx 131072 — and hold the tool-use verdict
for `tool_loop`, since single-turn emission will not separate them. Not
benchable here from the same survey: Tencent Hy4 preview (770B-A49B; the
1-bit GGUF is 229 GB against a 182.62 GiB budget); GLM-5.3-Flash stays
upstream-blocked (BLOCKED entry above).
Proposed 2026-09-06 — status: APPROVED 2026-09-09 (measurement) — MEASURED 2026-09-15 (below)

**Registered 2026-09-14.** `unsloth/Muse-Glimmer-30B-GGUF` Q8_0 at
`faa5b025c584` (the HF repo names it Muse Glimmer; architecture
`muse-glimmer`, 52 layers, no MTP head, 33.3 GB at 131072 context per
`explain`; Meta's sampling 1.0 / 0.95 / 64) and
`unsloth/NVIDIA-Nemotron-3.5-Lightning-30B-A3B-GGUF` Q8_0 at `f2d3fe369450`
(`nemotron_h_moe`, 53 blocks, ONE native MTP draft head — the speculative
question answered yes; NVIDIA's sampling 1.0 / 0.95). `explain` reports zero
KV bytes for the Nemotron hybrid because its Mamba-2 layers expose no
attention geometry to the sizing model; the cache is tiny on this
architecture, so the fit verdict stands, but the number is a gap. Both
registrations are local (`models.toml` is gitignored), pinned here by repo
and revision.

**Measured 2026-09-14/15 (MDT, 18:38–22:03).** `bench --suite agentic` on
each candidate under the loop-cap identity `7882af966fae`, read against
Ornith's run on that identity (`20260914T201544Z-ornith-1.5-35b-a3b`);
the built-in `compare` refuses across the differing registered contexts
and sampling, so the table is tallied from the rows as every receipt
before it. Runs `20260915T003820Z-nemotron-3.5-lightning-30b-a3b` (212
rows, 29 min) and `20260915T024259Z-muse-glimmer-30b` (211 rows, 81 min).
Raw evidence with a SHA-256 manifest:
[docs/agentic-campaign-20260915-tooluse-lane.tar.gz](docs/agentic-campaign-20260915-tooluse-lane.tar.gz).

| Axis (buffered) | Muse Glimmer 30B | Nemotron 3.5 Lightning | Ornith 1.5 35B A3B |
| --- | --- | --- | --- |
| tool_loop | 8/12 | 8/12 | 11/12 |
| instruction strict | 39/40 | 37/40 | 32/40 |
| tool_emit | 32/39 | 34/39 | 36/39 |
| grammar_gap | N/A (29 of 30 unavailable) | 2/30 (harness artifact) | 29/30 |

Both candidates are the strongest instruction followers measured on any
identity — Muse's 39/40 (40/40 streamed) and Nemotron's 37/40 beat GPT-OSS's
36/40 — and both are the weakest loopers after GPT. Of 92 axis/case pairs
graded on all three stacks, 25 distinguish, 65 pass everywhere and 2 fail
everywhere; five of twelve loop cases separate (`tl-003`, `tl-005`,
`tl-007`, `tl-009`, `tl-012`). The two candidates fail exactly the same four
loop cases, all by exhausting eight turns with a tool call on every turn:
the three test-gated edits (`tl-003`, `tl-009`, `tl-012`) and the missing-file
abstention (`tl-005`). Ornith closes all four. Both candidates pass the decoy
grep hit (`tl-007`) that Ornith and GPT fail, so the loop axis now separates
in both directions.

**Why the four (replayed, not inferred).** Rows record turn counts but not
the call sequence, so the four cases were replayed once against Muse
through the translator with the bench's own system text, palette and canned
tool behaviour (unseeded, so a reproduction of the shape, not the rows). All
four exhausted again. The shape: one call per turn, never a plain-text
reply. On the three test-gated cases the palette has no `list_dir`, and Muse
spends turns probing the layout through `read_file` on `.`, `src`,
`Cargo.toml` and `tests` (four of eight turns on `tl-012`, three on
`tl-009`), reaching `run_tests` only at turn seven. On `tl-003` it read,
ran the tests, edited the constant to the value the failure named at turn
six, saw `ok. 1 passed` at turn seven — and ran the tests again at turn
eight instead of reporting: the goal was met and the loop still failed,
because the design's only terminal state is a text reply. On `tl-005` it
listed, read the missing path, read every other file, and retried
`src/Legacy.rs` on turn eight without ever saying the file was missing. So
the shared failure is a habit both new stacks have and the three earlier
stacks lack: keep calling tools until the budget ends. Whether that habit is
the model or the eight-turn budget is a ruling, not a measurement.

**Caveats.** (a) Muse's forced arm returns the engine's `peg-native` parse
error (HTTP 500) on 29 of 30 cases, the same fault GPT-OSS hits on one loop
case; its grammar axis reads N/A and `bench` said so at launch. (b)
Nemotron's 2/30 is the forced arm's own 256-token cap: it thinks ~900
characters under the grammar and 28 of 30 forced replies stopped on
`max_tokens` with no answer — the same starvation the 2026-09-13 ruling
fixed on the other arms, on the one cap that ruling left alone. (c) Muse's
first run (`20260915T010705Z-muse-glimmer-30b`, in the tarball, one row
short) scored 1/40 strict because its entry carried `--reasoning-format
none`: Muse writes reasoning as a `to=self` message, and the engine's Muse
Glimmer parser (`llama.cpp/common/chat.cpp:3332`) only extracts it under the
default format, so the reasoning sat in the visible answer and every
line-count and fence check failed. The entry now carries no reasoning flag,
like GPT-OSS; the rerun is the measurement. (d) Muse is the first stack
whose verdicts differ across transports: `te-029` buffered thought 4323
characters into the 1024 tool cap and emitted no call (streamed: 2055 and a
pass), and `if-028` failed `contains:unchanged` buffered and passed
streamed — at temperature 1.0 the seeded doors do not sample alike. (e)
Muse's first download stalled at 7.7 GB and the pull sat on a dead
connection for three hours; the restarted pull finished in ten minutes.

**Verdict on the survey's question.** Neither candidate takes the tool-use
lane from Ornith on this bench: both lose the loop axis 8/12 to 11/12 and
single-turn emission 32–34/39 to 36/39, and win only the instruction axis.
The `all` + `--codebase` + `--judge` stage from the approval is not run: the
loop verdict the survey said to hold for is in, and it is against.

**Follow-ups — APPROVED 2026-09-15 (human approval in chat: "I approve"), shipped 2026-09-15 as `FORCED_MAX_TOKENS` in the identity hash, `calls` on the loop row and its failure line, and `[pull] stall_timeout_secs` (default 180) over a watched copy; re-measured the same night (below):**
1. Give the forced arm a named cap that rides in the agentic identity hash
   beside the other three (`caps=4096/1024/4096` today lists only those),
   and raise it to the tool cap; the grammar axis is blind on any thinker
   at 256, and the change refuses old comparisons by construction.
2. Record the per-turn tool names in the loop row (`["read_file",
   "run_tests", …]`, never the arguments): the replay above cost a server
   launch to learn what the row could have said.
3. The downloader has no read timeout (caveat e): a stalled connection
   should fail in minutes, not sit for hours.

**Measured 2026-09-15 (MDT, 23:02–01:47).** One `bench --suite agentic`
run over all five stacks under the new identity `e4cdb862d9e6`; runs
`20260915T050243Z-nemotron-3.5-lightning-30b-a3b`,
`20260915T052917Z-ornith-1.5-35b-a3b`, `20260915T054351Z-gpt-oss-120b`,
`20260915T055520Z-qwen3.5-9b`, `20260915T065124Z-muse-glimmer-30b`; 1060
rows, every row stamped, every loop row carrying its call trace. Raw
evidence with a SHA-256 manifest:
[docs/agentic-campaign-20260915-forcedcap.tar.gz](docs/agentic-campaign-20260915-forcedcap.tar.gz).

| Axis (buffered) | Muse Glimmer | Nemotron 3.5 L | Ornith 1.5 | GPT-OSS 120B | Qwen 3.5 9B |
| --- | --- | --- | --- | --- | --- |
| tool_loop | 8/12 | 8/12 | 11/12 | 7/11 (+1 unavailable) | 12/12 |
| instruction strict | 40/40 | 37/40 | 32/40 | 36/40 | 27/40 |
| tool_emit | 33/39 | 34/39 | 36/39 | 35/39 | 37/39 |
| grammar_gap | N/A (29 unavailable) | 25/30 | 29/30 | 28/30 | 29/30 |

Against the loop-cap identity, 25 verdicts changed across 1060 rows.
Twenty-three are Nemotron's grammar cases: 2/30 became 25/30, the ruling's
whole target; the four forced replies that still stop on `max_tokens` at
1024 think ~1250 characters under the grammar, and the fifth failure named
a tool outside the palette. Ornith, GPT-OSS and Qwen reproduced all 212
verdicts each, GPT's `tl-004` engine-side parse error included. The other
two are Muse's: `te-029` and `if-028`, its two transport asymmetries of the
previous run, now pass buffered as they did streamed — on requests the cap
change does not touch. Muse at temperature 1.0 is therefore the first stack
whose seeded sampling does not reproduce run to run; its scores here are a
sample, not a fixed point, and a Muse comparison needs more than one run.
Of 92 pairs graded on Muse, Nemotron and Ornith, 23 distinguish and 67 pass
everywhere; the same five loop cases separate.

The call traces say why the four shared loop failures happen, and they are
two habits, not one. Nemotron reads once or twice and then calls
`run_tests` six times in a row without ever calling `edit_file`
(`tl-003`: read, read, then six test runs; `tl-009`: test, read, then six
test runs), and lists directories seven times on the missing file. Muse
re-reads the same file six or seven times, runs the tests once, and never
replies in text (`tl-012`: five reads, a test run, a read, and its only
`edit_file` on turn eight). Neither trace shows progress toward the goal
being cut off by the budget, so the eight-turn cap is not the cause. For
the reference stacks: Ornith's decoy-hit failure is grep, read, edit, read,
edit, read — two edits of the call site — and GPT-OSS reads the same file
eight times on the already-done case and greps five times after one edit
on `tl-002`. Wall clock: Nemotron 26 min, Ornith 13, GPT 11, Qwen 55, Muse
56.

The verdict stands: neither candidate takes the tool-use lane from Ornith.

## Upstream engine work to watch, not build (2026-09-06)
Recorded so the next round does not re-research it. (a) Speculative prefill
(open llama.cpp PR, Aug 24–31 report): a draft model scores token
importance and only the "relevant" prompt chunks are prefilled — faster
long-context prefill "at the cost of some potential accuracy degradation",
i.e. NOT output-preserving. If it lands it belongs in the quality-graded
suites, never in `tune`, whose stages assume a flag changes speed and not
answers. (b) TurboQuant KV types (`turbo2/3/4`, 2–4-bit WHT-rotated KV,
~4.3x vs f16 at ~98% speed on Metal per the MLX port): fork-only as of the
Aug-2026 Debian `llama.cpp-tools` manpage and absent from the pinned
engine's `--cache-type-k` list (`f32, f16, bf16, q8_0, q4_0, q4_1, iq4_nl,
q5_0, q5_1`); when upstream merges they are one more `[tune] cache_types`
entry and the kv stage measures them — no chekov code. (c) Gemma 4 MTP
"assistant" drafter models (a separate GGUF; conversion support unmerged):
a draft-FILE path, which tune's spec stage deliberately does not model.
(d) `draft-eagle3`, `draft-dflash`, `draft-dspark` are in the pinned
engine's `--spec-type` list already, but each needs a trained draft head
shipped as a file — same reason, same deferral.
Proposed 2026-09-06 — status: APPROVED 2026-09-09 as DEFERRED (upstream) — revisit when upstream merges

## Time-to-first-visible-text — a design decision, not a build (2026-09-15)

**Question:** Should chekov measure elapsed time until the first visible
answer text, separately from throughput and thinking share? The reasoning
stamp records how much output is thinking (`thinking_chars`/`answer_chars`),
but those character counts do not measure when an answer becomes visible.

**Why the first proposal was withdrawn.** The earlier proposal added a
`to_first_visible` mark in `hub.rs` without first resolving the stream seam's
freeze and the metric's meaning. It remains withdrawn as an implementation
proposal. A new observation made while reading a stream could be honest;
reconstructing its arrival time from the completed response cannot be.

**What the current code measures (reviewed 2026-09-15).**

- `src/core/hub.rs:161-203` records `to_first_data`, elapsed time until a read
  first satisfies the SSE data predicate, and `first_to_done`, the remaining
  time until EOF. It retains the assembled body and whether it arrived in one
  read, not an arrival timestamp for each frame. These are client read times,
  not exact server token-generation times.
- On the client-timed path, `src/core/bench/runner.rs:518-552` computes prompt
  tokens divided by `to_first_data`, and completion tokens minus one divided
  by `first_to_done`. Both are rates in **tokens per second**, not latency.
  A first data event can contain metadata or reasoning before visible text;
  even a non-thinking model need not expose answer text in its first event.
- The llama.cpp streamed path instead reads the engine's timings object
  (`src/core/bench/runner.rs:364-393`), as does the buffered path. It is wrong
  to describe every reported `tok/s` value as derived from `to_first_data`.
  Foreign client timing is explicitly labeled in the report
  (`src/core/bench/store.rs:1101-1108`).
- `src/core/proxy/serve.rs:131-146` relays data through the translator without
  preserving arrival times. Parsing the final body in the runner can locate
  answer text but cannot recover when that text arrived. Thinking-character
  share is not a substitute for that missing observation.

**The gap to keep:** chekov does not yet report time to first visible answer
text. This is a missing latency measurement, not evidence that its existing
throughput units are mislabeled. The earlier reasoning-stamp decision left
this work deferred. Any new design must distinguish upstream arrival from
client-visible translated output; neither proves when a UI rendered it.

**Decisions to resolve:** Is visible-answer latency useful enough to change
model-selection decisions? If so, what observation point and narrowly scoped
change to the frozen seam should be authorized? A stored timestamp per frame
is one possible design, not a proven requirement: detecting the first visible
text during incremental reads may need only one additional mark. Metadata,
split tags, extracted reasoning, tool-only replies, empty replies, and reads
containing several events must all have explicit semantics. No answer means
unavailable, never a zero or an estimate from thinking share.

**Resolution paths:**

1. **Rule now:** retain the deferral and document that throughput and thinking
   share do not answer the visible-latency question.
2. **Research first (recommended):** spend at most one hour defining the
   observation point, tracing the existing paths read-only, and identifying
   a decision that would benefit from the new measurement. Return a proposed
   design and the exact freeze exception it would need; do not implement it.
3. **Spike first:** after a written, bounded exception for any frozen-file
   edits, spend at most one hour on `spike/first-visible-text` testing the
   observation with controlled streams. Never merge the spike. Report timing
   uncertainty and whether the result adds information beyond existing rates.

**Proposed research scope:** read the HTTP stream seam, bench runner, and
translator; change no production code, registry, configuration, or gates.
The recommendation does not approve a spike or supersede the existing design.

**More information / tags:** reasoning-stamp entry above;
`docs/superpowers/specs/2026-09-09-reasoning-stamp-design.md` §1c;
`pushkin-hub-freeze`. The historical claim of a roughly tenfold latency
change in a Qwen sweep has not been independently verified in this review and
is not an acceptance criterion or evidence authorizing implementation.

Proposed 2026-09-15 — status: OPEN as a design decision; the earlier build
proposal remains WITHDRAWN; research first recommended. A new charter is
conditional on confirming the current roadmap's completion: the capability
entry above still records fixture-v1 as release-gated, and its dated slice
status must be reconciled with the later delivery records before closure.

## fixture-v1 release-gate reconciliation and the gate it can't yet clear (2026-09-16)

**Question:** The capability-spec records `fixture-v1` as release-gated — it
does not ship compiled-in until it has been measured against three models of
clearly different capability with a real spread published. Today's
reconciliation asks: is that gate still in force, and if so, what is the
smallest next step?

**Why this was reopened.** The 2026-09-15 visible-text entry withholds its own
charter on the grounds that `fixture-v1` still records release-gated and its
dated-slice status must be reconciled with the later delivery records before
closure. So this record is the reconciliation it is waiting on.

**Reconciliation facts, checked 2026-09-16 against the tree, not the spec's
memory:**

- `fixture-v1` content does **not exist**. There are no compiled-in fixture
  templates, no `manifest.toml`, no fixture-v1 source tree. Only
  `--fixture <path>` (an external TOML the user points at) is wired in the
  bench CLI, and its `--fixture` flag still says "there is no compiled-in
  fixture yet."
- The three-model preflight (2026-09-11) measured the **agentic-v0 corpus** on
  three models with a real spread. It never ran on a compiled-in fixture,
  because there was none to run on. Angle A of the gate ("measured against three
  models of clearly different capability with the spread published") is therefore
  **unmet** — there is nothing to measure, not a measurement that failed.
- `gpt-oss-120b` (the F16 member of the preflight trio) is **gone** from
  `models/`; only `gpt-oss-20b` remains. The preflight set can no longer be
  reproduced as-is even once content exists.
- Fixture licensing (spec item #10: a fixture shipping in a public repo becomes
  training data) is still an open question, not a settled one.

**The reconciliation.** `fixture-v1` is still release-gated, and the gate has a
two-part precondition that does not exist yet. The gate cannot be cleared
against the *preflight corpus* — that corpus was never the fixture's subject,
and clearing the gate requires content that was never written. So the honest
status is: **unmet gate, pending a content slice that was never authored.**

**Decisions to resolve (only the human can answer these):**

1. **Author the content slice first (rule now / research first).** The gate
   cannot be approached until ~1,800 LOC of Rust fixture content plus a
   hidden-test manifest exist. Do we approve authoring that slice (a graded
   probe set for the compile / symbol-existence / near-miss API tiers), or is
   the fixture's purpose — no suitable repo, cross-machine comparability — no
   longer worth the work?
2. **License exposure (rule now / research first).** fixture-v1 ships in a public
   repo, so its templates become training data. The hidden-test design mitigates
   leakage of the *answers*, not the *prompts*. Do we accept that exposure, or is
   the fixture meant to be gated to codebase mode and never compiled-in?
3. **Acceptance threshold (research first).** "Clearly different capability with
   a real spread" — the preflight used Qwen-9B / Ornith-35B / GPT-OSS-120B. With
   120B gone, which three models define the spread today, and what spread value
   counts as "discriminating, not flat"?

**Recommended next step (research first, recommended):** before any gate,
author the fixture content slice as a bounded, separate decision (the
near-miss-API + invariant-trap + repo-symbol tiers the spec already sketches).
That content is the thing that does not exist; the gate can only be measured
against content. Do not run a campaign against the agentic-v0 corpus and call it
the fixture gate — that is a different measurement.

**Resolution paths:** rule now that the gate remains in force and the content
slice is the next artifact; research first (recommended) to author the slice
scope and settle the license question before a campaign; or spike a sample
fixture on a `spike/` branch to test discrimination, never merged.

**More information / tags:** capability-spec §9 (fixture mode), the
three-model-campaign preflight above, fixture.rs (`--fixture` external-only),
item #10 in the capability-spec's open decisions list. No production code,
registry, configuration, or gate changed by this record.

**Follow-up 2026-09-18.** The content slice this record asked for now exists:
`fixtures/fixture-v1/` (materialized ledger crate, four devices, held-out
assertions under `hidden/`, grading `manifest.toml`) landed on `develop` in
commits a8f9c74 (red) and f55ce68 (green). Decision 1 is therefore resolved by
delivery. Still open before the gate can be measured: decisions 2 (license
exposure) and 3 (which three models replace the preflight trio now that
`gpt-oss-120b` is gone, and what spread counts as discriminating). Also owed:
`manifest.toml` still reads `content_hash = "pending-materialization"`, and
nothing in `src/` reads the manifest yet — the assembler that withholds
`hidden/` from prompts and the grader that injects it are the next bounded
slice.

2026-09-18, later: the assembler and grader shipped — spec
`docs/superpowers/specs/2026-09-18-fixture-v1-compiled-in-design.md`. The
fixture is compiled in; the release gate (three models, published spread) is
the remaining step, and decisions 2 and 3 above are still open.

2026-09-18, final review: two of the four devices are weaker than
capability-spec §9 describes, and the release-gate campaign should read a flat
spread on those two as a fact about the fixture rather than about the models.
Device 1 needs no cross-file knowledge: `LimitedStore::record`'s masked body
resolves from the `capacity` field and the `StoreError::Full` variant already
named in the same file's doc comments, so it grades a capacity check, not
cross-file integration. Device 4 grades a `move` capture, not a signature: the
signature is given and only the body is masked, so the compile gate turns on
whether the predicate closure captures `filter` by move — a real trap, but a
narrower one than "generic + lifetime knot" implies (both the manifest label
and `fixtures/fixture-v1/README.md` now say `move`-capture closure). Devices 2
and 3 are unchanged and remain the discriminators. If a campaign returns a flat
spread on 1 and 4 while 2 and 3 separate the models, harden 1 and 4 out of the
ten reserve slots rather than concluding the candidates are equivalent.
Decisions 2 and 3 above remain open.

**Correction 2026-09-19.** The 2026-09-16 record above says `gpt-oss-120b` is
gone from `models/`. It is not: the registry entry is intact and its F16
weights sit at `/Volumes/jane/models/gpt-oss-120b@ff1a82da6ad4` (the
external volume that also holds the 27B, the 397B and MiniMax). The
preflight trio — Qwen-9B / Ornith-35B / GPT-OSS-120B — can be reproduced
as-is, so the first half of decision 3 is closed. Still open: the spread
threshold that counts as discriminating, and decision 2 (license exposure,
now moot in practice since the fixture is compiled in and public).

## fixture-v1 release gate — first measurement (2026-09-19)

**Question:** capability-spec §9 gates `fixture-v1` on three models of clearly
different capability showing a real spread. Merged as PR #99 today, the
fixture was run on three candidates under one stamp (`corpus
fixture-v1:78fc70e6de1c`, engine `dcacec736`, machine `c057455fb3a1`).

**Runs:** `eval/20260919T191534Z-qwen3.5-9b`,
`eval/20260919T192600Z-qwen3.8-27b`, `eval/20260919T191556Z-ornith-1.5-35b-a3b`.
`gpt-oss-120b` (`eval/20260919T191643Z-gpt-oss-120b`) recorded the codebase
lane as N/A: the model has no fill-in-the-middle tokens, and the managed
llama.cpp path grades the fixture over `/infill`. It cannot be a member of
this campaign; the 27B took its place.

| device | qwen3.5-9b | qwen3.8-27b | ornith-1.5-35b-a3b |
|---|---|---|---|
| 1 capacity check | pass | pass | pass |
| 2 near-miss API | wrong API (`append_entry`) | did not compile | wrong API (`append_entry`) |
| 3 exact cents | did not compile | pass | wrong result (integer parse) |
| 4 move capture | pass | pass | pass |
| tier 7 | 2 of 4 | 3 of 4 | 2 of 4 |

**Reading.** Tier 7 is not flat by the §9 band rule (0.67 / 1.00 / 0.50), so
the letter of the gate is met. The ordering is plausible: the 35B is a
mixture-of-experts model with ~3B active parameters. But the whole spread
rests on device 3 — devices 1 and 4 are passed by every model, device 2 is
failed by every model — and that is too thin to publish a number on.

**Ruling:** `fixture-v1` stays release-gated. The first measurement is
recorded here; the fixture is not cleared until at least two more devices
separate the 27B from the other two.

**Next slice — fixture hardening (proposed):** retire devices 1 and 4 into
the reserve slots; keep 2 (the near miss works — every model fell for it)
and 3 (the one device that discriminates); add devices shaped like 3, where
a stated contract has an obvious implementation that compiles and is wrong.
Wrap `src/domain/tests.rs` and refresh the `Cargo.toml.in` header in the same
re-hash (both flagged in PR #99). Trio for reruns: qwen3.5-9b, qwen3.8-27b,
ornith-1.5-35b-a3b — about a minute per model.

**Decisions closed by this record:** decision 3 (the trio is the three above;
the 120B is out for lack of FIM support, not for being gone); the spread
threshold is "at least two discriminating devices", not a tier-7 delta.
Decision 2 (license exposure) is moot: the fixture is compiled in and public.

## fixture-v1 hardening — first attempt failed acceptance (2026-09-19)

**What was done.** On `feat/fixture-v1-harden`: retired device-1 (capacity)
and device-4 (lifetime knot) into the reserve slots, kept device-2 (near-miss
API) and device-3 (exact cents), and added two new device-3-shaped traps —
device-5 (`Window::push`: newest-first + drop-oldest, contract in the module
doc) and device-6 (`Ledger::apply_entry`: a `Debit` of exactly the balance
must settle, not reject; contract on `LedgerError::InsufficientFunds`). Also
wrapped `src/domain/tests.rs` in a `#[cfg(test)]` module so it can never
surface as a visible answer key, and refreshed the `Cargo.toml.in` header.
New corpus id `fixture-v1:16f311a1699a` (was `78fc70e6de1c`). Both new devices
were pre-verified against real cargo in a scratch crate: gold passes, the
obvious wrong body compiles and fails the held-out test.

**Rerun (same stamp, temp 0, engine `dcacec736`, machine `c057455fb3a1`):**
`eval/20260919T213142Z-qwen3.5-9b`, `eval/20260919T213203Z-qwen3.8-27b`,
`eval/20260919T213323Z-ornith-1.5-35b-a3b`.

| device | qwen3.5-9b | qwen3.8-27b | ornith-1.5-35b-a3b |
|---|---|---|---|
| 2 near-miss API | test-fail | did not compile | test-fail |
| 3 exact cents | did not compile | pass | test-fail |
| 5 window slide | pass | did not compile | pass |
| 6 overdraft | did not compile | did not compile | did not compile |
| tier 7 | 1 of 4 | 1 of 4 | 1 of 4 |

**Reading — acceptance NOT met, and worse than the first measurement.**
Tier 7 is a flat 1/1/1. The 27B passes only device-3, exactly one device —
the target was two. Two failures made the new devices worthless as measured:

- *Device-6 is a compile sink.* All three models fail it on the **compile
  gate**, never reaching the boundary trap. `apply_entry`'s 15-line gold is
  followed by the near-identical `append_entry`; under `/infill` every model
  runs past the masked span and regenerates trailing methods, orphaning the
  suffix's closing brace (predictions 3585 / 3904 chars against 880 gold). It
  measures nothing.
- *Device-5 "discriminated" for the wrong reason.* qwen3.5 and ornith pass it
  cleanly; the 27B's only miss is the **same runaway** (958 chars, emitted new
  `pub fn`s), not the newest-first trap. Weakest+strongest pass, mid fails on
  a formatting artifact — noise, not signal.

**Root cause: FIM boundary runaway, not the traps.** The scratch verification
hand-wrote the bodies, so it proved the *traps* are sound but could not see
that a live model overruns the mask when a **similar-signature neighbour sits
in the suffix** (device-2's `handle_debit`, device-6's `append_entry`, the
window's sibling accessors). Device-3 survives because its neighbour
(`from_whole_dollars`) is dissimilar and its body ends cleanly. The first
measurement already showed this on device-2 (27B "did not compile"); the two
new devices inherited it.

**Ruling:** `fixture-v1` stays release-gated — the gate is NOT cleared. The
device change is a regression as-built and is not merged on its own.

**Next — runaway-resistant redesign (in progress):** reshape the new devices
so the masked body is short and self-contained and its file's *following*
lines are not a body a model will keep writing (dissimilar or no neighbour, or
end-of-impl). Re-verify against real cargo AND against the live trio before
proposing any gate ruling. The `tests.rs` answer-key wrapping and the
`Cargo.toml.in` refresh stand on their own regardless of the device outcome.

## fixture-v1 hardening — runaway-resistant redesign ready (2026-09-19)

The failed window/apply-entry pair above was not kept. Device 5 is now
`money::split_evenly`, a short integer-remainder body followed only by an
elided `#[cfg(test)]` module; device 6 is `ledger::debit_allowed`, a four-line
exhaustive match at end of file. Neither has a similar-signature method in its
suffix, so the boundary shape that caused the first rerun's runaway is gone.
The held-out split assertion covers positive, negative, exact, and zero-part
cases without exposing its values in prompt context; the debit assertion pins
equal, below, above, and credit cases. New corpus id:
`fixture-v1:068b719a1d81`.

Preflight against real cargo is green: all fixture unit tests and all four
held-out tests pass on the gold bodies; each new obvious wrong body still
passes `cargo check` and fails only its held-out tier-7 test. The assembler
resolves all four symbols and finds none of the gold or held-out answer
literals in its own prompt. The release gate remains closed until the same
three live models measure this corpus.
