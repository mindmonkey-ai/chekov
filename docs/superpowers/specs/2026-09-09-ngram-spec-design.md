# tune's spec stage trials the engine's n-gram drafters — design

Date: 2026-09-09. Status: **DRAFT — awaiting approval.** Implements the
IDEAS.md entry "tune's spec stage skips every model without an MTP head —
the engine has five drafter-free n-gram types" (2026-09-06, approved
2026-09-09). Touches ~9 files; the ">5 files — ask first" rule rides on the
same approval.

## 1. Purpose, and the evidence

`chekov tune`'s `spec` stage trials llama.cpp's native MTP draft head and
nothing else. Its first skip — "no MTP head in the GGUF
(`nextn_predict_layers 0`)" — fires on five of this registry's models
(`gpt-oss-20b`, `gpt-oss-120b`, `minimax-m2.7`, `gemma-3-12b-it`,
`qwen3.5-9b`; checked 2026-09-09 with `capability explain`), so for most of
what chekov serves the stage measures nothing.

The pinned engine (`0f194b907`) takes `--spec-type` as a comma-separated
list of eleven types tried in order, and five of them need no head and no
draft file: `ngram-simple`, `ngram-map-k`, `ngram-map-k4v`, `ngram-mod`,
`ngram-cache`. They draft from the token history — a run of tokens the
model has already seen, proposed again — and the target verifies every
draft, so greedy output is unchanged (`common/speculative.cpp`,
`common/ngram-map.cpp`: "speculative generation using the model's own
token history"). The server reports the drafted and accepted counts on its
`timings` object for every speculative type, which chekov's bench already
records per row (`draft_n`, `draft_n_accepted`) and prints as an acceptance
ratio.

What this buys is a speed measurement for a headless model on a repetitive
workload. What it cannot buy, and the design says so in every surface: an
n-gram drafter finds nothing to draft in text with no repetition. tune's
probe is 4096 tokens of filler plus "count upward" — the repetition it
carries is the counting itself, which is exactly the kind of run an n-gram
matcher accepts, so a hit is possible but unrepresentative. A win or a
loss on the probe says how the drafter behaves on that probe. The
confirming measurement is the one the 2026-09-03 MTP work used: the
candidate flags hand-applied and the model benched on real code, read
against the untuned run with `compare --cross-flags`, where the acceptance
ratio on the codebase rows is the number that answers "does it pay on my
work".

## 2. Command surface

Unchanged in shape. `chekov tune [NAME] [--stages spec,…]` — the `spec`
stage's candidate list grows a third spelling in config; no new flag.

## 3. The candidate grammar

`[tune] spec_drafts` entries are `off`, `mtp:<n>` (`n ≥ 1`), or
`ngram:<type>` where `<type>` is one of the five names above. Anything else
is `TuneBadSpecCandidate { value }` at plan time, as today. `SpecDraft`
gains `Ngram(NgramType)`, with `NgramType` a five-variant enum whose
`label()` is the engine's exact spelling; the config parser matches the
five names and nothing else (a sixth type the engine grows later is a code
change, deliberately — the engine's help is checked for the name at plan
time, §4, so a stale table cannot launch a candidate the engine refuses).

The default list stays `["off", "mtp:1", "mtp:2", "mtp:3"]`. A machine that
wants the n-gram trial opts in by adding, say, `"ngram:ngram-mod"` — the
same rule every tune knob follows: nothing changes until config says so.
`config.example.toml` shows the spelling in a comment. Why not default it:
each n-gram candidate is one more full launch (~5 min on the 35B), for a
measurement the probe is a poor instrument for (§1).

**Rewriting.** `apply(Stage::Spec, incumbent, value)`:

| value | `--spec-type` | `--spec-draft-n-max` |
|---|---|---|
| `off` | stripped | stripped |
| `mtp:<n>` | `draft-mtp` | `<n>` |
| `ngram:<type>` | `<type>` | stripped |

`--spec-draft-n-max` is the MTP head's draft length; the n-gram types have
their own knobs (`--spec-ngram-mod-n-max`, `--spec-ngram-simple-size-m`, …)
which this design does NOT tune and does NOT strip: whatever the incumbent
carries for them is left in place, so a user who has set
`--spec-ngram-mod-n-match` keeps it under the candidate. Stripping
`--spec-draft-n-max` on an n-gram candidate keeps a stale MTP length from
riding along on a flag set that no longer means it. `applied_extra_flags`
already copies the two speculative flags from a winner and strips them from
the current flags when the winner lacks them, so `--apply` needs no change:
an `ngram:<type>` winner writes `--spec-type <type>` and removes any
`--spec-draft-n-max`.

**The incumbent.** `spec_incumbent` today reads `draft-mtp` as `mtp:<n>` and
anything else as `off`. It gains the third case: `--spec-type <one n-gram
name>` reads as `ngram:<type>`, so a candidate equal to the incumbent is
filtered out as every stage does, and the record's `value` names it.

## 4. Skips

The three skips of the spec-stage design §4 become three narrower ones:

1. **No head in the weights** now skips only the `mtp:<n>` candidates; the
   reason gains a clause when the list also holds n-gram candidates: `no
   MTP head in the GGUF (nextn_predict_layers 0) — mtp candidates skipped,
   n-gram trialed`. `off` and every `ngram:<type>` still launch.
2. **Engine without the type.** The `--help` gate checks the candidate's
   own name in the `--spec-type` line, not just the flag: an engine whose
   list lacks `ngram-map-k4v` skips that candidate as `engine <commit> has
   no ngram-map-k4v in --spec-type — chekov update --engine`, and an engine
   with no `--spec-type` at all skips every candidate as today.
3. **A foreign speculative incumbent** narrows to what the stage genuinely
   cannot reason about: a `--spec-type` naming a draft-FILE type
   (`draft-simple`, `draft-eagle3`, `draft-dflash`, `draft-dspark`), a
   comma-separated chain, or `ngram-cache` with a `--lookup-cache-static`
   file. Reason: `the spec stage tunes draft-mtp and the n-gram types; the
   incumbent runs --spec-type <value>`. A plain n-gram incumbent is no
   longer foreign: it is the incumbent (§3).

The plan line's standing note becomes `needs an MTP head in the GGUF for
mtp candidates` when the list holds any.

## 5. What the verdict can say

The stage's judgement is unchanged: decode at `[bench] significance_pct`,
prefill not worse than the guard. Two additions to what is printed, both
from numbers the record already carries:

- Every measured trial's `stage_line` gains, for a speculative candidate,
  the acceptance clause the bench prints: `accept 63% (300 drafted)`, from
  the probe's `draft_n` / `draft_n_accepted`. A candidate that drafted
  nothing prints `no drafts` — the honest reading of an n-gram drafter on
  a prompt with nothing to match, and the line the reader needs to see
  before trusting "no significant difference".
- When any `ngram:` candidate ran, the report ends with one line: `n-gram
  drafting depends on repetition in the workload; the probe is a poor
  instrument for it — confirm with a codebase bench under the candidate's
  flags and compare --cross-flags`.

The record's `Trial` gains `draft_n` and `draft_n_accepted`
(`#[serde(default)]`, zero on every stored record, zero on a trial that
did not draft) beside `prompt_n`, so the clause is reproducible from disk.

## 6. The stamp and `compare`

Nothing. `Stamp.spec_type` already carries whatever `--spec-type` says, so a
run decoded under `ngram-mod` refuses against a plain one by name and
`--cross-flags` masks it; `spec_draft_n_max` reads `engine-default` on an
n-gram run, which is true. The bench's `speculative:` header already prints
the type and the summed acceptance.

## 7. `explain`

The nextn note is unchanged — it is about the head. `explain` gains no
n-gram note: there is nothing in the GGUF to read for it.

## 8. Configuration

`[tune] spec_drafts` accepts the third spelling (§3). No new key.

## 9. Errors

No new variants. A bad `ngram:<type>` is the existing
`TuneBadSpecCandidate`, whose remedy text gains the third spelling.

## 10. Testing

All inline `#[cfg(test)]` (tests/** is read-only):

- `core/tune.rs`: `SpecDraft::parse` accepts each of the five
  `ngram:<type>` spellings and refuses `ngram`, `ngram:`, `ngram:mtp`,
  `ngram:ngram-simple,ngram-mod`; `apply_spec` on `ngram:ngram-mod`
  rewrites `--spec-type` and strips `--spec-draft-n-max` while leaving
  `--spec-ngram-mod-n-match 24` in place; `applied_extra_flags` with an
  n-gram winner strips a current `--spec-draft-n-max`; a `Trial` from before
  the draft counts loads with zeros.
- `commands/tune.rs`: `spec_incumbent` reads `--spec-type ngram-mod` as
  `ngram:ngram-mod` and filters the matching candidate; the head gate skips
  `mtp:` candidates only, with the two-clause reason when n-gram candidates
  are listed; the engine gate skips exactly the candidate whose name the
  help lacks; the foreign skip fires on `draft-simple`, on a chain, and on
  `ngram-cache` with a static cache path, and not on `ngram-mod`; the plan
  line's note; the acceptance clause on a stage line (`accept 63% (300
  drafted)`, `no drafts`); the closing caution appears exactly when an
  n-gram candidate ran.
- `core/config.rs`: the default list is unchanged; a list with an n-gram
  entry parses.

Acceptance, live: `chekov tune gpt-oss-20b --stages spec` with
`spec_drafts = ["off", "ngram:ngram-mod", "ngram:ngram-simple"]` on this
desk — this needs the daily driver stopped, so it is the third item for the
same stop window as the 9B spread and the candidate bench. Until then the
plan's unit tests are the acceptance, and the CHANGELOG says the live
number is owed.

## 11. Out of scope

- Tuning the n-gram knobs themselves (`--spec-ngram-*`): five types times
  three knobs is a grid, not a stage.
- Chained types (`draft-mtp,ngram-mod`): a second grammar, as the idea
  said.
- The draft-file types; `explain` notes; a bench-side change.

## 12. Files

`src/core/tune.rs` (`SpecDraft`, `NgramType`, `apply_spec`, `Trial`),
`src/commands/tune.rs` (`spec_incumbent`, the three gates, `stage_line`
context, the plan note, the closing caution), `src/core/config.rs` (doc
comment), `src/error.rs` (the remedy text), `config.example.toml`,
`README.md` (the tune row and runbook sentence), `CHANGELOG.md`,
`IDEAS.md`, and the tune spec-stage design's §3/§4 status lines.
