# `chekov tune` spec stage — MTP draft decoding measured, never assumed — design

Date: 2026-09-01. Status: approved in chat 2026-09-01; this document is the
binding spec. Closes the second half of the IDEAS.md entry "MTP-head
awareness: `explain` reports it, bench measures it" (2026-08-30). The first
half shipped as `explain`'s nextn note (PR #51); this spec makes that note
true again and gives the head a measurement.

## 1. Purpose, and the evidence

Qwen 3.5+/3.8-family GGUFs — `ornith-1.5-35b-a3b` included — carry a native
multi-token-prediction draft head (`<arch>.nextn_predict_layers = 1`, tensors
`blk.40.nextn.*`, which survive the Q8_0 quant). The engine chekov builds
today (`0f194b907`, 2026-08-28) decodes with it when asked: `--spec-type
draft-mtp` runs the head against the main model's own weights (`common.cpp`:
"an MTP context runs on the weights of the main model") — no draft file, no
download.

Whether it PAYS is a machine-and-model question, and the engine's default
answers it wrong here. Spike 2026-09-01, this M3 Ultra, chekov's exact
launch argv for `ornith-1.5-35b-a3b` at ctx 262144, 512-token replies over
three coding/prose prompts, three repeats:

| launch | decode median (greedy / temp 0.6) | draft acceptance |
|---|---|---|
| baseline, no `--spec-type` | 71.2 / 71.0 tok/s | — |
| `draft-mtp`, `--spec-draft-n-max 3` (engine default) | 60.8 / 62.9 | 25–42% |
| `draft-mtp`, `--spec-draft-n-max 2` | 69.9 / 77.3 | 36–51% |
| `draft-mtp`, `--spec-draft-n-max 1` | 88.9 / 84.6 | 58–81% |

On a 3B-active MoE the trunk is cheap, so every rejected draft token costs
more than an accepted one saves; only the one-token draft keeps acceptance
high enough to win, and it wins by 20–25%. The gain is prompt-dependent (the
prose prompt gained least). That is exactly the shape `chekov tune` exists
for: a per-machine measurement with an honest verdict, not a hard-coded flag.

This spec adds a fifth tune stage, `spec`, that trials the head at each
configured draft length against the incumbent and keeps the incumbent unless
a candidate wins at `[bench] significance_pct`; names the speculative
configuration on the bench stamp so `compare` refuses to blend a run decoded
with the head against one decoded without it; and corrects `explain`'s note,
which currently claims the engine leaves the head idle.

## 2. Command surface

Unchanged in shape. `chekov tune [NAME] [--dry-run] [--yes] [--apply]
[--stages spec,fa,kv,batch,ubatch]` — `spec` is a fifth label, accepted by
`--stages` and listed in its help text and in the README's command table
row. Absent `--stages`, all five stages run in the fixed order of §3.

## 3. The stage

`Stage::Spec` is added FIRST in `Stage::ORDER`:

| stage | flags rewritten on the incumbent argv | candidates (`[tune]` key) | primary metric | why |
|---|---|---|---|---|
| `spec` | `--spec-type` and `--spec-draft-n-max` together | `spec_drafts = ["off", "mtp:1", "mtp:2", "mtp:3"]` | decode | speculative decoding trades draft cost against accepted tokens per step; only decode can see it |

It runs first because the four existing stages tune the kernel and batch
geometry around whatever decode path is active, and the head changes that
path. `--stages` still lets a reader run it alone or last.

**Candidate grammar.** Each `spec_drafts` entry is `off` or `mtp:<n>` with
`n ≥ 1`. Anything else is `TuneBadSpecCandidate { value }` at plan time —
before the confirm gate, before any launch — because the value has to be
split into two flags and a string the engine would merely reject is not a
measurement. Existing lists (`flash_attn`, `cache_types`) stay unvalidated:
their values are passed through verbatim and the engine's own refusal is
already a recorded outcome.

**Rewriting.** `apply(Stage::Spec, incumbent, value)`:

- `mtp:<n>` → `rewrite(--spec-type, "draft-mtp")` then
  `rewrite(--spec-draft-n-max, "<n>")` — the existing `rewrite` semantics
  (first occurrence replaced in place, later duplicates dropped, absent flag
  appended under its long spelling).
- `off` → both flags STRIPPED, with their values, wherever they appear. A new
  `strip(argv, flag)` helper does this; it is the first tune rewrite that
  removes a flag, and it exists because "no speculative decoding" is the
  absence of `--spec-type`, not a value of it.

`Flag` gains `SpecType` and `SpecDraftNMax`. Neither has a short spelling, so
`Flag::names` changes from `[&str; 2]` to `&'static [&'static str]` — one
entry for these two, two for the existing five; `rewrite`'s "append under the
long spelling" appends the LAST name, which is the long spelling for every
flag.

**The incumbent's value** (`incumbent_value(Stage::Spec, argv)`):

- `--spec-type` absent → `off` (the engine default; `engine_default(Spec)`
  is `"off"`).
- `--spec-type` carrying `draft-mtp` → `mtp:<n>` where `<n>` is
  `--spec-draft-n-max`'s value, or the engine's own default when the flag is
  absent (`ENGINE_DEFAULT_SPEC_DRAFT_N_MAX = 3`, per `llama-server --help`,
  kept beside `ENGINE_DEFAULT_BATCH` for the same one-fact reason).
- `--spec-type` carrying anything else (an ngram type, a separate-draft
  type, a comma list) → the stage is skipped as a whole, §4.

As for every stage, a candidate equal to the incumbent's value is not
launched — it IS the incumbent's number — and `planned()` says so in the
plan line's parenthetical.

**Winning** is unchanged: `stats::compare` at `[bench] significance_pct`
says `Faster` on decode and not `Slower` on prefill. Prefill is the guard
that matters here — an MTP context is a second graph on the same weights,
and the stage must not buy decode with a prefill regression it never looked
at. `pick_winner`, ties, `defaults won`, the record and `--apply` all apply
as written in the tune spec.

## 4. Skips — the stage is honest about what it cannot measure

Three pre-launch skips, each recorded as `Outcome::Skipped(reason)` per
candidate exactly like `kv_skip` and `fa_skip` (before any spawn; costs
nothing; does not shrink `max_launches` or the printed estimate, for the
reason the tune spec §4 gives):

1. **No head in the weights.** The plan reads the model's first shard header
   through `gguf::read_geometry` (the same path `explain` takes) — when
   `nextn_predict_layers` is absent or 0, every `mtp:<n>` candidate is
   `Skipped("no MTP head in the GGUF (nextn_predict_layers 0)")`. `off` is
   then the incumbent (or a real candidate, if someone hand-wrote
   `--spec-type draft-mtp` for a model without a head — in which case the
   engine's refusal is the recorded outcome, as today).
2. **Engine without the flag.** The engine's own `--help` — the same scan
   the flag-hygiene assertion reads before every spawn — has no
   `--spec-type`: every candidate is `Skipped("engine <commit> has no
   --spec-type — chekov update --engine")`. Without this the assertion would
   raise `BenchFlagUnknown` and stop the whole run, which is the wrong
   outcome for a stage that is merely unavailable.
3. **A foreign speculative incumbent.** `--spec-type` set to anything other
   than `draft-mtp` alone: every candidate is `Skipped("the spec stage tunes
   draft-mtp only; the incumbent runs --spec-type <value>")`. chekov does not
   guess what a user's ngram or separate-draft configuration is worth.

A skipped stage prints its lines and moves on; the descent continues with
the incumbent unchanged.

## 5. Plan, estimate, report

The plan line follows `stage_plan_line`'s shape:

```
  spec       4 candidates   (1 is the incumbent; needs an MTP head in the GGUF)
```

The parenthetical's second clause is the stage's standing note, as `kv` and
`ubatch` have theirs. Skips 1–3 are discovered at plan time too, and when one
applies the plan line says so instead of the note:

```
  spec       4 candidates   (skipped: no MTP head in the GGUF (nextn_predict_layers 0))
```

and the skipped candidates contribute 0 to `max_launches` — the ceiling stays
a ceiling. (Under skip 1 an `off` candidate against a hand-written draft-mtp
incumbent is still a launch, and is still counted.)

Report lines are `stage_line` as it exists (`spec  mtp:1  <cells>  <phrase>`),
and the verdict block, record and `--apply` diff are untouched in shape.

**`--apply`** writes the winner's speculative flags the same way it writes
the others — `applied_extra_flags` rewrites each flag the winner carries —
with one addition: a winner that carries NO `--spec-type` strips
`--spec-type` and `--spec-draft-n-max` from the model's current `extra_flags`
if they are there. The winner is a full argv derived from the current flags,
so "absent in the winner" means the stage removed it (an `off` win), never
"the stage did not run" — a stage that did not run leaves the flags on the
winner exactly as it found them.

## 6. The bench stamp

`Stamp` gains two flag-sourced fields immediately after `flash_attn`:

```rust
/// `--spec-type` as the argv said it, "engine-default" when absent (no
/// speculative decoding), "unmanaged" on a foreign run.
#[serde(default = "engine_default_flag")]
pub spec_type: String,
/// `--spec-draft-n-max` likewise — only meaningful beside a draft
/// `spec_type`, recorded regardless so two runs never differ silently.
#[serde(default = "engine_default_flag")]
pub spec_draft_n_max: String,
```

Every stored run predates the fields and every one of them was decoded
without speculation, which is what the serde default says. `first_mismatch`
grows to 25 pairs in declaration order — `spec_type` and `spec_draft_n_max`
sit after `flash_attn` and before `allow_exec`, so a pair that differs only
there is refused by name: `stamp mismatch: spec_type "engine-default" vs
"draft-mtp"`. That refusal is the point: a run decoded with the head and a
run decoded without it are different environments, and `compare` must not
average them.

**One definition of the launch flags.** Today the six flag-sourced values
are read in two places with identical name pairs — `stamped_flags` in
`capability.rs` and `tune::sextet` in `core/tune.rs` — and adding two more
to both is how they drift. This spec moves the set to `stamp.rs`:

```rust
pub struct LaunchFlags { kv_unified, n_batch, n_ubatch, type_k, type_v,
                         flash_attn, spec_type, spec_draft_n_max }   // all String
pub fn launch_flags(argv: &[String]) -> LaunchFlags
pub fn unmanaged_flags() -> LaunchFlags   // every field "unmanaged"
```

`StampedFlags`/`stamped_flags`/`unmanaged_flags` in `capability.rs` and
`FlagSextet`/`sextet` in `core/tune.rs` are replaced by these. The tune
record's `Trial.stamp` becomes a `LaunchFlags`; the two new fields carry
`#[serde(default = "engine_default_flag")]` so every record under `tune/`
still loads (`deny_unknown_fields` only refuses unknown keys, and the JSON
shape of the six existing keys is unchanged).

**`--cross-runtime`** masks the flag sextet today because a foreign server's
flags are unobservable; the two new fields join `CROSS_RUNTIME_ALLOWED` (14
entries) and `mask_cross_runtime` for the same reason, and the banner names
them when they differ.

**The report** (`render_run`) gains one line after the timing-source line,
printed only when the run was decoded speculatively, in the pattern of
`timing_source_line`:

```
speculative: draft-mtp, draft length 1
```

`spec_draft_n_max` of `engine-default` beside a draft `spec_type` prints
`draft length engine-default`. A run with `spec_type` `engine-default` prints
nothing here — its render is what it always was.

## 7. `explain`

`render_geometry`'s nextn note changes from

```
  nextn_predict_layers    1   (a native MTP draft head; this engine decodes without it)
```

to

```
  nextn_predict_layers    1   (a native MTP draft head; `chekov tune --stages spec` measures whether it pays)
```

`explain` reads only the GGUF and must not claim what the engine does with
the head; pointing at the measurement is the honest note. The zero case is
unchanged (no parenthetical).

## 8. Configuration

`[tune]` gains one key, defaulted, `deny_unknown_fields` as before:

```toml
[tune]
depth = 4096
spec_drafts = ["off", "mtp:1", "mtp:2", "mtp:3"]   # stage spec: off, or draft-mtp at that draft length
flash_attn = ["on", "off"]
cache_types = ["q8_0", "f16"]
batch_sizes = [512, 1024, 2048, 4096]
ubatch_sizes = [256, 512, 1024, 2048]
```

`config.example.toml` and the README's config block carry the new line with
that comment. The README's `[tune]` prose says six keys and five stages.

## 9. Errors

- `TuneBadSpecCandidate { value }` — `[tune] spec_drafts entry '<value>' —
  expected "off" or "mtp:<n>" with n ≥ 1`. Raised at plan time, before the
  confirm gate.
- Everything else reuses the tune and bench errors as today. An engine that
  starts and then dies under a spec candidate is `ServerDiedWhileLoading`
  per trial (recorded, not fatal), exactly as for any other candidate.

## 10. Testing

Unit tests in the modules they touch; every quoted string above is asserted
verbatim.

- `core/tune.rs`: `Stage::ORDER` is `[Spec, Fa, Kv, Batch, Ubatch]` and
  `spec` parses; `mtp:<n>` rewrites both flags together and `off` strips
  both wherever they sit (including a `--spec-type` in the middle of the argv
  with a value, and an argv without either flag, which is returned equal);
  the incumbent is not a candidate under either spelling of "off" (absent
  flags, or `--spec-type` absent with a stray `--spec-draft-n-max`); a bad
  entry is the named error; `applied_extra_flags` strips the two flags when
  the winner lacks `--spec-type` and rewrites them when it carries them, and
  leaves them alone when the winner carries what current carries.
- `commands/tune.rs`: `incumbent_value(Spec)` reads `off`, `mtp:<n>`, and
  `mtp:3` for a draft-mtp incumbent without `--spec-draft-n-max`; each of
  the three skips is a `Skipped` outcome per candidate and never a spawn
  (the existing `NoHttp` scratch context proves no HTTP happened); the plan
  line carries the note, the skipped form, and a 0 contribution to the
  ceiling; a spec winner threads into the record and `--apply` writes the
  two flags; an `off` win removes them from `extra_flags` through
  `Registry::save`.
- `core/bench/stamp.rs`: the two fields default to `engine-default` on a
  stamp without them; `spec_type` differs after `flash_attn` and before
  `allow_exec`; `launch_flags` reads all eight and `unmanaged_flags` is
  eight sentinels.
- `core/bench/compare.rs`: a pair differing only on `spec_type` is refused
  by name; `--cross-runtime` masks exactly the 14-entry allow-list and the
  banner names a differing `spec_type`.
- `core/bench/store.rs`: the speculative line prints for a draft run with
  its draft length, and is absent for an `engine-default` run.
- `commands/capability.rs`: `render_geometry` carries the new note for a
  head and none for zero.
- `core/config.rs`: `[tune]` defaults carry the four-entry list and a file
  setting `spec_drafts` overrides it; an unknown key is still refused.
- The integration tests under `tests/` are write-protected and construct no
  `Stamp`, `TuneSection` or `Trial` by literal (checked 2026-09-01), so no
  new field can force an edit there.

## 11. Out of scope

- A `run`-time default for speculative decoding: what `tune --apply` writes
  is the only path, on purpose — the spike shows the wrong draft length is a
  15% loss.
- The separate-draft spec types (`draft-simple`, `draft-eagle3`,
  `draft-dflash`, `draft-dspark`) and the ngram types: they need a second
  artefact or have no head to gate on; skip 3 names them and stops.
- A bench-side speculative row (accept rate by depth): the tune record and
  the stamp already say what was measured under; `/metrics`'
  `spec_decode_num_draft_tokens_total` counters can feed a later slice.
- The 9B small-lane face-off (IDEAS.md, survey 2026-08-30): unrelated, still
  unrun.

## 12. Files

`src/core/tune.rs` (the stage, `strip`, the spec grammar, the shared flag set
moved out), `src/commands/tune.rs` (incumbent value, the three skips, the
plan line, `--apply` stripping, `--stages` help), `src/core/bench/stamp.rs`
(two fields, `LaunchFlags`, `launch_flags`, `unmanaged_flags`,
`first_mismatch` 25), `src/core/bench/compare.rs` (allow-list 14, mask),
`src/core/bench/store.rs` (the speculative line), `src/commands/capability.rs`
(the note; `StampedFlags` replaced), `src/core/config.rs` (`spec_drafts`),
`src/error.rs` (one error), `README.md`, `config.example.toml`,
`CHANGELOG.md`, `IDEAS.md` (the MTP entry's status; the spike table),
regenerated `shell/_chekov` via `make install` if the completions embed the
`--stages` help text.
