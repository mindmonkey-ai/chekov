# Reasoning effort on the stamp, thinking share on the row — design

Date: 2026-09-09. Status: **DRAFT — awaiting approval.** Implements the
IDEAS.md entry "Reasoning effort is a launch flag nobody stamps, and a cost
nobody measures" (2026-09-06, approved 2026-09-09). Touches ~10 files; the
">5 files — ask first" rule rides on the same approval.

## 1. Purpose, and the evidence

Two runs that differ only in how much the model was allowed to think compare
today as the same environment. The pinned engine (`0f194b907`, checked
2026-09-06) takes four launch flags that change that: `--reasoning
[on|off|auto]`, `--reasoning-effort LEVEL` (`minimal` … `max`),
`--reasoning-budget N` (`0` ends thinking at once) and `--reasoning-format
FORMAT` (where the thoughts land: `reasoning_content`, or left in `content`
as a `<think>` span). Any of them can sit in a registry entry's `extra_flags`
right now — the daily driver runs `--reasoning-format none` — and none of
them reaches the bench `Stamp`, so `compare` would read the difference
between a `low` run and a `high` run as a verdict on the model.

And nothing measures the cost. The 2026-08-31 foreign run lost every chat
fill to reasoning before the operator disabled thinking server-side; every
Qwen3.8-27B guide says the model overthinks by default; a 45-configuration
sweep found reasoning effort moved time-to-visible-answer ~10x, more than
any server flag. chekov's report has no number that sees it: a backend that
is right but spends 80% of its reply thinking looks, on every line the
report prints, like one that answers.

This design does two things and refuses a third. (a) The four reasoning
flags join the launch flags the stamp reads off the argv, so `compare`
refuses across them by name and `--cross-flags` can mask them like the other
eight. (b) Every probe row records how many characters of the upstream reply
were thinking and how many were answer, read where the thinking still exists
— the `OpenAI` body before translation, which is the only place a
`--reasoning-format none` run's `<think>` span survives — and the report
prints one `thinking` line per run with the share per suite. (c)
Time-to-first-visible-text is NOT built: the mark would have to come from the
streamed-read loop in `hub.rs`, which is frozen for agents on this repo
(`pushkin-hub-freeze`), and a share-derived estimate would be a number
chekov did not measure.

## 2. Command surface

Unchanged. `compare` refuses on a differing reasoning field exactly as it
refuses on `type_k`; `--cross-flags` masks twelve fields instead of eight and
its banner names any that differ; `--cross-runtime` masks them too (a
foreign server's reasoning flags are as unobservable as its KV flags). The
report gains one line. No new config key, no new CLI flag: the flags are set
where every launch flag is set, in `extra_flags`.

## 3. The stamp

`stamp::LaunchFlags` gains four fields after `spec_draft_n_max`, read by
`launch_flags` with the spellings llama-server accepts, `"engine-default"`
when absent, `"unmanaged"` on a foreign run:

| field | flag spellings |
|---|---|
| `reasoning` | `--reasoning`, `-rea` |
| `reasoning_format` | `--reasoning-format` |
| `reasoning_effort` | `--reasoning-effort` |
| `reasoning_budget` | `--reasoning-budget` |

`Stamp` gains the same four, `#[serde(default = "engine_default_flag")]`,
placed after `spec_draft_n_max` and before `allow_exec` in both the struct
and `first_mismatch`'s declaration-order table (25 → 29 entries). Every
stored `stamp.json` and every tune record under `tune/` loads unchanged: a
run recorded before the fields was launched with whatever `extra_flags`
said, and "engine-default" is the honest reading of a field nobody wrote —
the report says so once, in the `thinking` line's footnote, when it renders
such a run.

`compare`: `CROSS_FLAGS_ALLOWED` grows to twelve, `CROSS_RUNTIME_ALLOWED` to
eighteen, `mask_cross_flags` and `mask_cross_runtime` copy the four, and
both banners already iterate their allow-list, so a differing
`reasoning_effort` prints as `reasoning_effort: "low" vs "high"` with no
further code. The tune record's per-trial `stamp` field carries the four
too (the same `LaunchFlags`), so a tune run's flags are read in the same
words as a bench run's; tune's stages do not tune them (§8).

## 4. The measurement

`runner::Timings` gains two counts, read beside the token counts:

- `thinking_chars: u64` — `reasoning_content`'s characters plus the
  characters inside every `<think>…</think>` span in `content` (tags
  excluded), so a `--reasoning-format none` run counts its thinking exactly
  as a `deepseek`-format run does.
- `answer_chars: u64` — `content` with the spans removed, plus every
  `tool_calls[].function.arguments` string. A tool call IS the answer for a
  tool probe; counting only text would read a perfect `tool_emit` reply as
  all thinking.

Read on every door that has a body: the buffered door from
`choices[0].message` in `timings_from`; the streamed doors by folding the
frames' `delta.reasoning_content`, `delta.content` and
`delta.tool_calls[].function.arguments` in `stream_timings` (llama.cpp) and
`timings_from_stream` (foreign) — the content deltas concatenated first,
then the span scan, so a `<think>` tag split across two chunks still counts.
An untimed crossing (`cross_untimed`) records no measure and so no share,
as today.

Characters, not tokens, and the report says so. llama-server reports no
reasoning token count anywhere in `usage` or `timings` (checked in
`tools/server/` at the pinned commit), and tokenizing every reply twice
would double the probe's wire cost for a number the share already
approximates within the tokenizer's own variance. The share is the same
ratio either way to within a few points on the same model, and it is the
ratio the reader wants.

`store::Measure` gains `thinking_chars` and `answer_chars`
(`#[serde(default)]`, so every stored row loads as zero-both), copied by
`probe_measure` and summed by the loop driver's `fold` like the draft
counts. Rows written before this change read "no measurement" (both zero),
never "no thinking".

## 5. The report line

One line in `render_run`, after the suite summaries and before the
asymmetry lines, printed only when at least one row of the run measured any
characters:

```
thinking     share of reply characters spent thinking, median per case: tool_emit 12%, grammar_gap 0%, instruction 61%, tool_loop 48%, throughput 9%
```

Per suite: over the rows with `thinking_chars + answer_chars > 0`, the
median of `thinking / (thinking + answer)`, as a whole percent. A suite with
no such rows is omitted from the line; a run with none prints no line. When
the stamp's four reasoning fields are all `engine-default` the line ends
with ` (launched with no reasoning flag)` so a reader knows the share is the
template's own default, not a chosen effort. `compare` gets the stamp
refusal and the banner from §3 and nothing more in this slice — a side-by-
side share belongs with the next change that touches `compare`'s agentic
section.

## 6. What the row and the record carry

- `TaskRow.measure.thinking_chars` / `answer_chars` — every suite's rows,
  both doors, zero where the crossing was untimed.
- `Stamp.reasoning`, `reasoning_format`, `reasoning_effort`,
  `reasoning_budget` — once per run.
- `tune/<run>.json` trials: the four fields inside each trial's `stamp`.

No schema version bump: every addition is a serde default.

## 7. Errors

None new. A body without `choices[0].message` already fails `read_timings`
as `BenchNoTimings`; the character counts default to zero on a message that
has no `content`, which is a reply with nothing in it, not an error.

## 8. Out of scope

- Time-to-first-visible-text (§1c). The IDEAS entry keeps it as owed, with
  the `hub.rs` freeze named as the reason.
- A tune stage for reasoning effort: it changes answers, not just speed, so
  it fails tune's premise; the bench's quality suites are where it is
  judged, and this change is what makes two such runs comparable by name.
- A per-entry registry key: `extra_flags` already is one.
- A `compare` column for the share (§5).
- Token-exact counts via `/tokenize`.

## 9. Testing

All inline `#[cfg(test)]` (tests/** is read-only):

- `stamp.rs`: `launch_flags` reads all twelve, each reasoning spelling
  (`-rea` and `--reasoning`) resolves; `unmanaged_flags` is twelve
  sentinels; a stamp JSON without the four fields loads as engine-default; a
  differing `reasoning_effort` is named by `first_mismatch` after
  `spec_draft_n_max` and before `allow_exec`; a pre-change tune-record
  `LaunchFlags` JSON (eight fields) still loads.
- `compare.rs`: two stamps differing only in `reasoning_budget` refuse
  naming it; `--cross-flags` masks it and the banner prints
  `reasoning_budget: "0" vs "engine-default"`; `--cross-runtime` masks it.
- `runner.rs`: `timings_from` counts `reasoning_content`; counts a
  `<think>` span inside `content` under `none` format and leaves the tags
  out; counts tool-call arguments as answer; the streamed fold counts a span
  split across two chunks; the foreign stream path counts deltas the same
  way.
- `store.rs`: a row without the fields loads zero-both; `probe_measure`
  copies both; the `thinking` line prints per-suite medians, omits a suite
  with no measured rows, prints nothing for a run with none, and appends
  the no-flag footnote when the stamp is all engine-default.
- `toolloop.rs`: `fold` sums the two counts across turns.
- `tune.rs`: a trial's record stamp carries the four fields read off its
  argv.

Acceptance, live: rerun `--suite agentic` on the daily driver (reused) and
read the `thinking` line — the driver runs `--reasoning-format none`, so a
non-zero share there is the `<think>`-span path proving itself.

## 10. Files

`src/core/bench/stamp.rs`, `src/core/bench/compare.rs`,
`src/core/bench/runner.rs`, `src/core/bench/store.rs`,
`src/core/bench/toolloop.rs` (the fold), `src/core/bench/codebase/run.rs`
(`probe_measure`, `empty_measure`), `src/commands/tune.rs` (a record test),
plus every `Timings` / `Measure` literal the compiler names; `README.md`
(the `compare` row's "eight" becomes "twelve", the bench row's report
description gains the line), `CHANGELOG.md`, `IDEAS.md` (status line).
