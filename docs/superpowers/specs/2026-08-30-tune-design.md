# `chekov tune` — per-machine launch-flag autotune with an honest verdict

Date: 2026-08-30. Ships the IDEAS.md entry of the same name (proposed 2026-08-30).
Builds on the bench lifecycle (`launch_candidate` → `ensure_ready` → `teardown_candidate`),
the throughput sweep (`sweep::measure_depth`, `stats::summarize`, `stats::compare`) and the
configuration stamp. Status: DESIGN — approved in outline 2026-08-30; awaiting review of
this document. Nothing below is implemented.

## 1. Why, and what it deliberately leaves out

`models.toml` carries one launch argv per model, written by hand for the machine it was
written on. Whether `--flash-attn on`, a `q8_0` KV cache or a `--batch-size 4096` helps or
hurts depends on the chip, its bandwidth and its thermals — and nobody re-measures when the
model moves to another Mac. `chekov tune` measures, on **this** machine, whether any of a
small set of launch flags beats what the model launches with today, and says **`defaults
won`** when nothing does. The model is the honest-verdict pattern `mtplx tune` uses: keep
the baseline, save a candidate only when it beats it, save nothing otherwise, and never
persist a measurement that is garbage.

What the research pass changed in the outline (2026-08-30):

- **Each axis is judged on the number it can move.** On Metal, decode is bandwidth-bound;
  `--batch-size` / `--ubatch-size` govern prompt processing and barely touch decode, while
  the KV cache type and flash attention move decode (and memory). A single decode verdict
  would have declared the batch axes noise. Every stage names its primary metric (§4).
- **A degenerate trial can never win or be saved** — `mtplx`'s later rule ("tune can no
  longer persist garbage"). §5 defines degenerate.
- **Thermal state is recorded from what macOS will say without root.** `pmset -g therm`
  prints a `CPU_Speed_Limit` only when the machine is throttled and "no thermal warning
  level has been recorded" otherwise; the true signal (`com.apple.system.thermalpressurelevel`)
  is a C notification API, unreachable under `#![forbid(unsafe_code)]`. §6.

Left out, and said in every report:

- A full grid (`--exhaustive`): 4 × 4 × 2 × 2 = 64 launches is hours on a 35B model; the
  four-stage descent below is at most 9 (§7). Deferred until a descent verdict is shown
  to be wrong.
- Any axis beyond the four (`--threads`, `--n-gpu-layers`, `--kv-unified`, `--fit`,
  speculative flags): each is a new measurement question with its own metric.
- Tuning several models in one invocation: `tune` takes one model; a loop is the shell's.
- Writing `models.toml` without `--apply`: the run records; the human decides.

## 2. Command surface

```
chekov tune [NAME] [--dry-run] [--yes] [--apply] [--stages fa,kv,batch,ubatch]
```

- `NAME` defaults to the active model. The model must pass `run`'s preflight for **every**
  trial (shard present, port free, no server running, the footprint gate — a `q8_0`→`f16`
  KV trial can push a model over the budget, and that trial is then `Skipped("exceeds the
  GPU budget by N MiB")`, never launched).
- `--dry-run` prints the stage plan, the trial count and the wall-clock estimate as data
  (§7) and launches nothing.
- `--yes` pre-approves the confirm gate. Every trial is a launch, so a run without `--yes`
  confirms once, up front, with the estimate.
- `--apply` writes the winner into the model's `extra_flags` after a second confirm that
  prints the exact diff (§8). Without it the winning argv is printed for hand-copying.
  `--apply` on a `defaults won` run has nothing to write and says so.
- `--stages` restricts the descent to the named stages, in the fixed order (§4); the
  default is all four.
- A running server chekov did not start is a refusal (`BenchWrongModel`'s rule: tune never
  stops a server it did not start). A running server chekov did start is named in the plan
  and stopped — `teardown` with the budget-release check — only after the confirm gate and
  before the baseline trial, never silently; it is not restarted afterwards, and the report
  says so.

## 3. Measurement

One **probe** is `sweep::measure_depth` at `[tune] depth` (default 4096) with `[bench]
repetitions` (default 5, first sample dropped as warmup) through `runner::cross` — the same
Anthropic-shaped throughput probe bench uses, so a tune number and a bench number are the
same kind of number. It yields two `stats::Summary` values: **decode** tok/s and
**prefill** tok/s, medians with p10–p90.

One **trial** is: build the candidate argv (§4) → `lifecycle::unknown_flags` against
`llama-server --help` (an unknown flag is a `BenchFlagUnknown` refusal before any spawn) →
the footprint gate for the candidate's KV type → `launch_candidate` (Metal residency env,
pidfile, run state) → `ensure_ready` (`/health` then `/props` asserting `n_ctx`) → thermal
reading → probe → thermal reading → `teardown_candidate` with the budget-release check.
The **baseline** is trial 0: the model's current effective flags, unchanged.

The lifecycle trio moves from `src/commands/capability.rs` into `src/core/bench/candidate.rs`
(`launch`, `ensure_ready`, `teardown`, unchanged in behaviour) so `bench` and `tune` cannot
drift; `bench` keeps calling them.

## 4. The descent

Four stages, in a fixed order, each starting from the **incumbent** (the baseline, then
whatever has won so far). Within a stage every candidate is measured once; the stage's
winner — if any — becomes the incumbent for the next stage.

| stage | flag rewritten on the incumbent argv | candidates (`[tune]` keys) | primary metric | why this metric |
|---|---|---|---|---|
| `fa` | `--flash-attn` | `flash_attn = ["on", "off"]` | decode | attention kernel choice moves per-token cost; `fa off` needs unquantized KV — an `off` candidate under a quantized-V incumbent is `Skipped("fa off requires unquantized KV — llama.cpp refuses the combination; skipped under a <v-cache> incumbent")` |
| `kv` | `--cache-type-k` and `--cache-type-v` together | `cache_types = ["q8_0", "f16"]` | decode | cache width moves bytes read per token; `q8_0` needs FA on — a `q8_0` candidate under an FA-off incumbent is `Skipped("q8_0 KV needs flash attention on")` |
| `batch` | `--batch-size` | `batch_sizes = [512, 1024, 2048, 4096]` | prefill | the logical batch bounds prompt processing |
| `ubatch` | `--ubatch-size` | `ubatch_sizes = [256, 512, 1024, 2048]`, only values ≤ the incumbent batch | prefill | the physical batch is the Metal dispatch size |

**The fa-off skip mirrors the kv-skip rule in the other direction (evidence 2026-09-01,
IDEAS.md).** `chekov tune`'s own kv stage can leave the incumbent with a quantized V cache
(`q8_0`, most commonly) once it wins; llama.cpp then refuses to start at all with
`--flash-attn off`, exiting at load with "quantized V cache requires flash_attn to be
enabled" — the engine names the V cache specifically, which is why the skip keys on
`-ctv`/`--cache-type-v` (and would key on `-ctk` too if the engine ever required that
instead — it currently does not). Like `kv_skip`, `fa_skip` runs before any spawn and costs
nothing; also like `kv_skip`, it does not shrink `max_launches` or the printed estimate —
`planned()` counts a stage's candidates against the argv the plan is drawn up with, not the
incumbent that will actually be active once earlier stages have run, so a skip discovered at
descent time is invisible to the upfront ceiling for both rules alike.

Rewriting is by flag name: the incumbent's value for the flag (wherever it appears in the
argv — `defaults.flags` or `extra_flags`; the engine's short spellings `-fa`, `-ctk`,
`-ctv`, `-b`, `-ub` count as the same flag) is replaced; an absent flag is appended. A
candidate identical to the incumbent is not measured again — it *is* the incumbent's number.

**A candidate wins its stage** only when, against the incumbent, `stats::compare` at
`[bench] significance_pct` says `Faster` on the stage's primary metric **and** not `Slower`
on the other metric. Two candidates that both win are ordered by primary median; ties keep
the earlier. `NoSignificantDifference` keeps the incumbent — the report prints the two
medians and the intervals so the reader sees how close it was. A stage where nothing wins
prints `<stage>: incumbent kept`.

The run's verdict is **`defaults won`** when no stage changed the incumbent, else the final
incumbent argv with the baseline's and the winner's decode and prefill summaries side by
side and the per-stage path that got there.

## 5. Degenerate trials — never a winner, never saved

A trial is **degenerate**, recorded with its reason and excluded from every comparison, when:

- the server never became ready (`ServerDiedWhileLoading`, readiness timeout) — the
  candidate flags may be the cause, so the run continues to the next candidate rather than
  aborting; the reason is printed;
- the probe failed or any repetition returned no `timings` (`BenchNoTimings`);
- fewer than 2 samples survived the warmup drop (`summarize` returned `None`);
- the measured `prompt_n` is below half the requested depth (the engine truncated or cached
  the prompt; the number is not the depth's number).

A degenerate **baseline** ends the run: there is nothing to compare against, and the error
names the trial's reason. A run whose every candidate is degenerate is `defaults won` with
every reason listed — the honest reading of "nothing beat the baseline".

## 6. Thermal honesty

Before and after every probe, `pmset -g therm` is read (no root needed). Its
`CPU_Speed_Limit = N` line, when present, is recorded as `speed_limit_pct: Some(N)`; the
"no thermal warning level has been recorded" form is `None`, printed as `nominal (no
warning recorded)`. A trial whose either reading is below 100 carries `clock was dirty
(CPU_Speed_Limit N%)` on its report line and in the record; it still counts — a throttled
machine's tune is still that machine's tune — but the reader knows. The limitation is
stated in the record's `thermal_source: "pmset -g therm"` field: macOS exposes the real
pressure level only through a C notification API this crate does not link.

## 7. Plan, estimate and confirm

`--dry-run` prints:

```
tune ornith-1.5-35b-a3b @ ctx 262144, probe depth 4096 × 5 reps
  baseline   --flash-attn on --cache-type-k q8_0 --cache-type-v q8_0   (current flags)
  fa         2 candidates   (1 is the incumbent)
  kv         2 candidates   (1 is the incumbent; the f16 KV is footprint-gated per trial)
  batch      4 candidates   (1 is the incumbent)
  ubatch     4 candidates   (1 is the incumbent; values ≤ the incumbent batch)
  ≤ 9 launches, ~33 min estimated (load ≈ 4 s/GiB × 35.2 GiB, probe ≈ 40 s each)
```

The count is an upper bound — the baseline plus one launch per non-incumbent candidate
(1 + 1 + 1 + 3 + 3 with the default lists): a stage's winner can shrink the next stage
(`ubatch` values above a smaller winning batch drop out), and skipped candidates cost
nothing. The estimate uses `lifecycle`'s load rate and the probe's own arithmetic; it is
printed as an estimate.
Any launch requires the confirm gate or `--yes`; the prompt is the plan's last line.

## 8. The record and `--apply`

Every run writes `<root>/tune/<utc_compact>-<model>.json`:

```json
{
  "model": "ornith-1.5-35b-a3b", "quant": "Q8_0", "revision": "fbbaed45c2f0",
  "machine_id": "8d41f0c2a917", "engine_build_commit": "0f194b907", "chekov_version": "0.1.0",
  "probe": { "depth": 4096, "repetitions": 5, "max_tokens": 128 },
  "thermal_source": "pmset -g therm",
  "trials": [
    { "stage": "baseline", "argv": ["--flash-attn","on","--cache-type-k","q8_0","--cache-type-v","q8_0"],
      "stamp": { "n_batch": "engine-default", "n_ubatch": "engine-default", "type_k": "q8_0", "type_v": "q8_0", "flash_attn": "on" },
      "decode": { "median": 31.2, "p10": 30.8, "p90": 31.6, "n": 4 }, "prefill": { "median": 402.1, "p10": 398.0, "p90": 405.2, "n": 4 },
      "prompt_n": 4101, "speed_limit_pct": [null, null], "outcome": "measured" },
    { "stage": "fa", "argv": ["--flash-attn","off", "..."], "outcome": "measured", "verdict": "slower on decode (24.9 vs 31.2)" },
    { "stage": "kv", "argv": ["..."], "outcome": "skipped", "reason": "exceeds the GPU budget by 4120 MiB" }
  ],
  "winner": null,
  "verdict": "defaults won"
}
```

The stamp fields are the bench stamp's flag sextet (`stamp::flag_value` over the trial's
argv) so a tune trial and a bench run describe a configuration in the same words. `winner`
is the final incumbent's argv when it differs from the baseline, else `null`.

`--apply`: after the run, when `winner` is not null, print the `extra_flags` diff for the
model's `models.toml` entry — the winner's four flags replacing or joining the existing
list, everything else untouched — and confirm (`--yes` covers it). Then write through
`Registry::save` (atomic, as every registry write) and print the line `applied — the next
\`chekov run <name>\` launches with these flags`. Nothing is ever applied to `defaults.flags`:
a per-machine win for one model is not a default for every model.

## 9. Report

```
tune ornith-1.5-35b-a3b (Q8_0@fbbaed45c2f0) — machine 8d41f0c2a917, engine 0f194b907, probe depth 4096 × 5
  baseline   decode 31.2 [30.8..31.6]  prefill 402 [398..405]   --flash-attn on --cache-type-k q8_0 --cache-type-v q8_0
  fa         off      decode 24.9 [24.1..25.5]  prefill 397 [390..401]   slower on decode — incumbent kept
  kv         f16      skipped: exceeds the GPU budget by 4120 MiB
  batch      512      prefill 288 [280..295]   slower on prefill
             1024     prefill 361 [355..366]   slower on prefill
             4096     prefill 431 [427..436]   faster on prefill, decode not slower — new incumbent
  ubatch     256      prefill 402 [396..409]   slower on prefill
             1024     prefill 466 [461..470]   faster on prefill — new incumbent   clock was dirty (CPU_Speed_Limit 87%)
             2048     prefill 468 [459..474]   no significant difference vs 466 — incumbent kept
  winner     --flash-attn on --cache-type-k q8_0 --cache-type-v q8_0 --batch-size 4096 --ubatch-size 1024
             decode 31.1 vs 31.2 (no significant difference)   prefill 466 vs 402 (+16%)
  record     tune/20260830T190000Z-ornith-1.5-35b-a3b.json
  apply with: chekov tune ornith-1.5-35b-a3b --apply   (or add the flags to extra_flags by hand)
```

`defaults won` replaces the `winner` block with one line: `defaults won — no candidate beat
the current flags at p < 5% on its metric` followed by the record path.

## 10. Configuration

A new `[tune]` section (`deny_unknown_fields`, every key defaulted):

```toml
[tune]
depth = 4096                              # probe prompt depth in tokens
flash_attn = ["on", "off"]                # stage fa
cache_types = ["q8_0", "f16"]             # stage kv (applied to K and V together)
batch_sizes = [512, 1024, 2048, 4096]     # stage batch
ubatch_sizes = [256, 512, 1024, 2048]     # stage ubatch (≤ the incumbent batch)
```

`repetitions`, `max_tokens` and `significance_pct` come from `[bench]` — one definition of
"how many samples" and "what is significant" for every measurement chekov makes.

## 11. Errors

- `TuneBaselineDegenerate { name, reason }` — the baseline could not be measured; nothing to
  compare against. Remediation: `chekov run <name>` and `chekov doctor` first.
- `TuneNothingToApply { name }` — `--apply` on a `defaults won` run.
- Everything else reuses the bench errors: `BenchWrongModel`, `BenchFlagUnknown`,
  `ServerDiedWhileLoading` (per trial, recorded, not fatal), `BenchBudgetNotReleased`
  (fatal: a trial's memory was not returned, the next one would measure a contended
  machine), `ModelExceedsBudget` (per trial → skipped).

## 12. Tests

- Argv rewriting: replace a long or short spelling, append when absent, rewrite K and V
  together, an unchanged candidate is recognised as the incumbent.
- Stage plan: candidate lists from config; `ubatch` filtered by the incumbent batch; the
  `q8_0`-under-FA-off skip; `--stages` order is the fixed order regardless of the argument's.
- Verdict: `Faster`-on-primary-and-not-`Slower`-on-secondary wins; `Faster` on primary but
  `Slower` on secondary does not; two winners ordered by median; `NoSignificantDifference`
  keeps the incumbent; the printed phrases are exact.
- Degenerate: each §5 condition classified with its reason; a degenerate candidate is
  excluded; a degenerate baseline is the named error; all-degenerate is `defaults won` with
  reasons.
- Thermal: `pmset` output with `CPU_Speed_Limit = 87` → `Some(87)`; the "no warning" form →
  `None`; unreadable → `None` with `thermal_source` still recorded.
- Record: round-trip; the stamp sextet from a trial argv; `winner` null on `defaults won`.
- Apply: the `extra_flags` diff for an entry with and without the flags present;
  `defaults.flags` untouched; `TuneNothingToApply`.
- Estimate and dry-run: exact lines for a two-stage plan.
- Live (PR body): `chekov tune ornith-1.5-35b-a3b --yes` on this machine, the full report,
  the record file, and — if a winner exists — the `--apply` diff (not applied).

## 13. Files

`src/commands/tune.rs` (new: the command, plan/confirm, the loop, the report),
`src/core/tune.rs` (new: stages, argv rewriting, verdict, degenerate rule, thermal parse,
record types — pure), `src/core/bench/candidate.rs` (new: the lifecycle trio moved from
`capability.rs`), `src/commands/capability.rs` (call the moved trio), `src/core/config.rs`
(`[tune]`), `src/cli.rs` and `src/commands/mod.rs` (the subcommand), `src/error.rs` (two
errors), `README.md` (command table row, config block), `config.example.toml`,
`CHANGELOG.md`, `IDEAS.md`, regenerated `shell/_chekov` completions via `make install`
(never hand-edited). Roughly 900 LOC with tests.
