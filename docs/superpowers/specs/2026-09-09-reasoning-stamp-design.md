# Reasoning effort on the stamp, thinking share on the row — design

Date: 2026-09-09. Status: approved in chat 2026-09-09; this document is the
binding spec. Implements the IDEAS.md entry "Reasoning effort is a launch
flag nobody stamps, and a cost nobody measures" (2026-09-06, approved
2026-09-09). Touches ~10 files; the ">5 files — ask first" rule was
satisfied by the same approval.

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

## 11. Amendments after the seam map and the three-lens critique (2026-09-09)

A read-only workflow (four seam readers, three adversarial critics) reviewed
this design against the code before any plan was written. What changes:

- **Seven reasoning-side launch flags, not four.** llama-server at the
  pinned commit also takes `--reasoning-budget-message`,
  `--reasoning-preserve` / `--no-reasoning-preserve`, and
  `--chat-template-kwargs` — the last is how `enable_thinking` is switched
  off on Qwen-family templates, and `--reasoning on|off` is the same kwarg
  by another spelling. Two runs differing on any of them think differently.
  `LaunchFlags` and `Stamp` gain `reasoning`, `reasoning_format`,
  `reasoning_effort`, `reasoning_budget`, `reasoning_budget_message`,
  `reasoning_preserve` (`on` / `off` / engine-default; `--no-…` reads as
  `off`) and `chat_template_kwargs` (the verbatim string). Fifteen
  flag-sourced fields; `--cross-flags` masks fifteen, `--cross-runtime`
  twenty-one; `first_mismatch` is split so the flag block is its own
  function under the 40-line gate.
- **The flag reader accepts negative numbers.** `--reasoning-budget -1` is
  llama-server's own spelling of "unrestricted"; today `flag_value` reads a
  next token starting with `-` as a bare switch and would stamp `on`. A next
  token that parses as an integer is a value.
- **Stored runs are hydrated at load, never defaulted.** Every llama.cpp run
  under `eval/` (35 of 41) was launched with `--reasoning-format none` and
  says so in `stamp.json`'s `launch_args`, one key above the stamp. A serde
  default of `engine-default` would make all of them refuse to compare with,
  or resume under, any run made after this change. Instead the run-head
  reader re-derives all fifteen flag-sourced fields from `launch_args`
  (`launch_flags`) when the runtime is llama.cpp and sets them `unmanaged`
  otherwise — idempotent for fresh stamps, which were written from the same
  argv, and it also corrects the two speculative fields on the stored
  foreign runs, which today load as engine-default while a fresh foreign
  run stamps `unmanaged`. The serde default remains only for a head with no
  argv at all. `assemble_stamp` and the loader copy the flags through one
  `Stamp::set_flags`, so they cannot disagree.
- **An unclosed thinking span is thinking to the end of `content`.** Under
  `--reasoning-format none` the parser's own terminators include
  `<tool_call>`, so a reply that thinks and then calls a tool carries
  `<think>` with no `</think>`; a reply cut by `max_tokens` mid-thought has
  none either. Both are exactly the rows the measure exists for. An opened
  span with no close counts to the end of the text as thinking; tool-call
  arguments still count as answer. Every span counts, not only the first.
- **The span scan is a table, and it is not only `<think>`.** Under `none`,
  llama.cpp leaves each family's own tags inline: `<think>` (Qwen, Ornith,
  MiniMax, DeepSeek), `[THINK]…[/THINK]` (Mistral), gpt-oss's
  `<|channel|>analysis<|message|>…<|end|>`, Gemma's `<|channel>thought…
  <channel|>`, `<mm:think>`. The scanner reads a const table of (start,
  end-tags) pairs mirrored from `common/chat.cpp`'s `thinking_start_tag` /
  `thinking_end_tags` at the pinned commit — the `<think>` entry reusing
  the proxy's `THINK_OPEN` / `THINK_CLOSE` so the two spellings cannot
  drift — and the CHANGELOG names the table's contents so a new family is
  a known gap, not a silent 0%.
- **Foreign reasoning fields.** The stream fold reads `delta.reasoning`
  as an alias of `delta.reasoning_content` (the spelling other
  OpenAI-compatible servers use). On a foreign run the `thinking` line's
  footnote reads `(reasoning flags unmanaged on this runtime)`.
- **The forced arm is named.** `Stamp.reasoning_format` is the server-wide
  launch value; the grammar arm's per-request `deepseek` override stays in
  `RunHead.forced_reasoning_format`, and the `thinking` line appends
  `; grammar_gap measured with reasoning extracted (deepseek)` when it is
  set. The live acceptance reads the unconstrained suites' share, which is
  the `<think>`-span path; `grammar_gap`'s share is the extracted path.
- **Where the fold lives.** `timings_from_stream` never sees the SSE, and
  `stream_timings` reads one frame. One helper, `stream_reply_chars(sse)`,
  folds every frame's `delta.reasoning_content` / `delta.reasoning`,
  `delta.content` (concatenated, then scanned) and
  `delta.tool_calls[].function.arguments`; `cross_streaming` and
  `cross_stream_timed` call it and write the counts onto their `Timings`.
  The buffered door reads `choices[0].message` through `reply_chars`.
- **The report line lives in `suite_summaries`** (which is what calls
  `asymmetry_lines`), after the streamed `tool_loop` line; it is a
  `thinking_line(log) -> Option<String>` shaped like `speculative_line`.
  The median is a new `stats::median` (sort, middle, no warmup drop);
  `stats::summarize` is never used for it. `codebase` appears on the line
  only for rows the chat-FIM transport timed; `/infill` rows carry no
  message and stay zero-both.
- **Characters are `chars().count()`**, and the claim that the character
  share tracks a token share "within a few points" is withdrawn: thinking
  is prose and a tool reply is JSON, and they tokenize differently, so the
  tool suites' share is biased and the README says the share is over
  characters. An injected `--reasoning-budget-message` counts as thinking.
- **Files.** §10 gains `src/commands/capability.rs` (`assemble_stamp`),
  `src/core/bench/sweep.rs` and `speeds.rs` (test literals),
  `src/core/stats.rs` (`median`), `src/core/tune.rs` (the record test lives
  with `Trial`), the README's three "eight" mentions (the compare row, the
  cross-runtime paragraph, the flags paragraph) and the doc comments and
  test names that say eight, twenty-five or fourteen.
