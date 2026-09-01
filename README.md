# chekov

[![ci](https://github.com/mindmonkey-ai/chekov/actions/workflows/ci.yml/badge.svg)](https://github.com/mindmonkey-ai/chekov/actions/workflows/ci.yml)
[![license: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Local llama.cpp inference stack manager for Apple Silicon Macs with enough
unified memory to run large GGUF models (developed on a 256 GB Mac Studio;
anything from 32 GB up works with an appropriately sized model). One static binary owns the full lifecycle —
**pull → run → stop/restart → status → doctor → update** — with models
abstracted behind a registry so adding one is a single `pull`, ollama-style.
Integrates with zsh, Hermes Agent, and Claude Code.

- Package name: `chekov-mac` (crates.io `chekov` is taken); binary: `chekov`
- No async runtime; blocking `ureq` for the HF API, downloads, and llama-server
- Every failure is loud and names its remediation command; nothing degrades
  silently (no auto-shrunk ctx, no model fallback, no skipped checks)

## Requirements

- macOS on Apple Silicon, Xcode CLT, `cmake` and `git` (Homebrew is fine)
- Rust stable (pinned by `rust-toolchain.toml`; rustup will honor it)
- Disk for weights (~160 GB per large model) — external volumes supported
  via `--model-loc`

## Installation

### From crates.io

```sh
cargo install chekov-mac   # installs the `chekov` binary
chekov setup               # clone + Metal-build llama.cpp under ~/.chekov
```

The root directory (registry, logs, engine checkout, default weights dir) is
`~/.chekov`; set `CHEKOV_HOME` to put it elsewhere.

Prebuilt arm64 tarballs (binary + zsh shim + completions) are attached to
each [GitHub Release](https://github.com/mindmonkey-ai/chekov/releases).

### From source

```sh
git clone https://github.com/mindmonkey-ai/chekov.git && cd chekov
cp config.example.toml config.toml   # optional: tune wired_limit_mb, port, …

make setup      # cargo release build + clone/cmake llama.cpp with Metal
                # (builds llama-server, llama-cli, llama-gguf-split)

make install    # cargo install --path .  → chekov on PATH (~/.cargo/bin)
                # generates zsh completions (shell/_chekov)
                # appends ONE idempotent source line to ~/.zshrc

exec zsh        # pick up PATH, `cclocal` alias, tab completion
```

When installed from source, the clone directory is the chekov root:
`shell/chekov.zsh` exports `CHEKOV_HOME` pointing at itself, so registry,
logs, weights (unless `--model-loc`), and the llama.cpp checkout all live
under the clone. If you move the checkout, re-run `make install`.

`setup` ends by reading the GPU budget — what `llama-server --list-devices`
reports, falling back to `iogpu.wired_limit_mb` (a sysctl value of `0` means
"macOS system default", resolved as **75% of RAM**, never treated as zero).
There is no built-in floor: on any Apple Silicon Mac, `run` judges each
model's own footprint (weights + KV cache at its context) against that budget
and refuses only a model that does not fit, naming the levers that exist
(a smaller quant, a lower `ctx_size`, `chekov capability recommend`). If you
set `[limits] wired_limit_mb` in `config.toml`, setup and `run` verify the
budget against that floor instead and print the exact
`sudo sysctl iogpu.wired_limit_mb=<N>` command when it is short. **chekov never
executes sudo.**

## Quickstart

```sh
chekov pull unsloth/MiniMax-M2.7-GGUF:UD-Q5_K_XL
chekov use minimax-m2.7
chekov run                # starts in the background by default
chekov doctor            # six health checks; non-zero exit on any failure
```

Already have the weights on an external drive (huggingface-cli layout)?

```sh
chekov pull unsloth/MiniMax-M2.7-GGUF:UD-Q5_K_XL --model-loc /Volumes/external/models
```

Files found under `<loc>/<RepoName>/<rfilename>` (or `<loc>/<rfilename>`)
are **size-verified against the Hugging Face API and hard-linked** into
`<loc>/<name>@<rev12>/` — instant, zero extra space, and a truncated shard
can never be adopted silently (a mismatch warns and downloads instead).
The registry stores the absolute path; everything else works unchanged.

## Command reference

| Command | What it does |
|---|---|
| `run [name] [--foreground]` | Start llama-server (default: active model). Backgrounds by default; `--foreground` blocks the terminal instead. Refuses loudly if: shard missing, port occupied, wired limit below config, engine not built, or a server already running. |
| `stop [--if-running]` | SIGTERM via pidfile, 20 s grace, SIGKILL escalation with a warning. Detects and cleans stale pidfiles. Stopping a stopped server is an error (exit 1) unless `--if-running`, which prints "nothing to stop" and exits 0 — for idempotent teardown scripts. |
| `restart [name]` | Stop (if running) then start in the background; swaps models in one motion. |
| `status` | running/pid, model, revision, port, ctx, uptime, wired-limit actual (with system-default annotation) vs required, log tail path. |
| `capability [scan] [--json]` | What this Mac is and what it can hold: chip, GPU cores, performance threads, macOS, and the GPU budget **with its provenance**. `scan` is the default action, so the bare command is the scan; `--json` emits it as JSON instead of a table. The budget is read from the engine (`llama-server --list-devices`) when it is built, from `iogpu.wired_limit_mb` when set, and only otherwise from the 75%-of-RAM formula — which measures 31457 MiB low on a 256 GiB M3 Ultra, so the source is always printed. |
| `capability graph [--metric fit\|tok-s] [--svg [PATH]] [--ctx N]...` | Grid of registered models against context lengths. Each cell is two characters: the fit verdict, then whether its inputs were measured or predicted. A predicted GPU ceiling is announced in the header and changes the legend, because every verdict below it is then measured against a guess. `--metric tok-s` turns the first character into a band digit 1–9 of the **measured** decode median from the stored runs under `eval/` — fixed band edges printed in the legend, an exact model+quant+ctx+machine match required, and `??` wherever no run exists: predicted and measured are never blended in one column. `--svg` writes the same frontier as a self-contained SVG (bare, into `reports/`); the path is **printed, never opened**. |
| `capability recommend [--ctx N] [--role agent\|chat] [--refresh] [--limit N]` | Ranks the registered models for this machine. Gates first — anything that exceeds the budget, or cannot be sized, is listed with its reason rather than dropped. Then sorts: under `--role agent` a model whose chat template has no dedicated llama.cpp tool parser is **downranked with a note, not refused**; under `--role chat` the tool parser is ignored. `--refresh` is the **only** networked path — without it chekov ranks registered models only and never reaches out. |
| `capability explain [name] [--ctx N]` | Read one model's GGUF header and print its fit arithmetic line by line: block count, the MTP/interval layer ladder, padded context, cache type, KV bytes, weights on disk. Local file read; no network. |
| `tune [NAME] [--dry-run] [--yes] [--apply] [--stages fa,kv,batch,ubatch]` | Measure, on **this** machine, whether any of a small set of launch flags beats what the model launches with today. A four-stage descent — `fa` (`--flash-attn`) then `kv` (`--cache-type-k`/`-v`) judged on **decode**, then `batch` (`--batch-size`) then `ubatch` (`--ubatch-size`) judged on **prefill** — starting from the model's own flags as the baseline; a candidate wins its stage only when `stats::compare` at `[bench] significance_pct` says `Faster` on the stage's metric and not `Slower` on the other. No stage winning is printed and recorded as **`defaults won`** — the honest verdict when nothing beats the current flags. Every run writes a JSON record under `tune/<utc>-<model>.json` (trials, verdicts, the significance threshold, thermal readings). Thermal state is read from `pmset -g therm` before and after every probe (no root needed) and noted on any trial where the clock was dirty. `--apply` prints the exact `extra_flags` diff for the model's `models.toml` entry and, after a confirm (`--yes` covers it), writes the winner through `Registry::save`; a `defaults won` run has nothing to apply and says so. `--dry-run` prints the stage plan, an upper-bound launch count and a wall-clock estimate without launching anything; `--stages` restricts the descent to the named stages in their fixed order. |
| `capability bench [--models a,b] [--suite throughput\|agentic\|all] [--fixture F] [--resume RUN] [--dry-run] [--yes] [--allow-exec] [--judge NAME] [--runtime NAME@VERSION] [--upstream URL]` | Measure candidates through chekov's own Anthropic↔OpenAI translator and store every run. `--models` takes a **comma-separated** list and benches them **sequentially**: bench launches and tears down its own server behind `run`'s preflight gates and **never stops a server it did not start** — a running server is reused only when it *is* the single request, and is otherwise a refusal. `--suite` defaults to `throughput`: the depth sweep over `[bench] depths`, `[bench] repetitions` per depth with the first dropped as warmup. `agentic` runs the probe set (`tool_emit`, `grammar_gap`, `instruction`), with each unconstrained case crossing both the buffered and the streamed door so an asymmetry between them is named. `all` runs both. Each run lands in `eval/<timestamp>-<model>/` as `stamp.json` (the configuration stamp plus the exact launch argv) and `results.jsonl` (one flushed append per task), so a crash loses at most one task and `--resume <RUN>` skips what that run already holds — resuming under a changed stamp is refused. `--dry-run` prints the plan and a rough wall-clock estimate as data; `--yes` pre-approves the launch confirmation; `--fixture` supplies graded probes from your own TOML (there is deliberately no compiled-in fixture). `--judge` names a registered `role = "judge"` model of a different family from every candidate, loaded once after they are all down; it answers one position-swapped, grammar-forced binary question per `function_body` crossing, and the `equiv` column is voided below `[bench] judge_min_consistency_pct`. The 2026-08-30 probe recommends `gpt-oss-20b` (Apache-2.0); Gemma 3 12B also clears the gate. `--runtime <name>@<version>` [`--upstream <url>`] benches a foreign OpenAI-compatible server you already started instead of chekov's own llama-server — see **Foreign runtimes** below. |
| `capability bench --codebase <PATH>` | The repository at PATH (clean tree required) as 24 deterministic infill tasks, sampled from HEAD (`[bench] codebase_tasks`, split 12 `in_file` / 6 `function_body` / 6 `cross_file_first`), run through `/infill`, graded on tiers 1–5 (exact, edit similarity, identifier F1, parse, repo-symbol existence); tiers 6–7 (compile gate, covering test) run only under `--allow-exec`, which is the single gate on every path that executes repository code. A `cross_file_first` task masks the first use in a file of a symbol defined in **another** file, and is crossed **twice** — without that file and with it in `input_extra` — so the report can print what reading the repository buys. Masks are boundary-scanned, not AST, and the report says so. A model without FIM tokens is N/A, never zero. Given without `--suite`, the codebase corpus is the whole run — the throughput sweep does not come along. |
| `capability compare <A> <B> [--cross-runtime]` | Compare two stored bench runs, named by run id under `eval/` or by run directory, across all three sections: **throughput** per depth, **agentic** (the report's own pass counts side by side over the cases both runs graded, then the disagreements — the cases exactly one run passed, with the losing side's reason; cases graded in only one run are named, never dropped), and **codebase** (per tier group and ladder tier: both means, a signed delta, per-task win counts, and an exact two-sided binomial sign test at p < 0.05). **Same environment only**: it refuses on the FIRST differing stamp field and names it, because llama.cpp does not promise bit-identical results across configurations. The subject fields (`weights_revision`, `quant`) are exempt — they are what is being compared — and a differing task set always refuses. Verdicts name the model, never "A"/"B"; `no significant difference` is a first-class printed outcome, never resolved into a winner; a section one run never measured says so rather than vanishing. `--cross-runtime` relaxes the refusal for a named allow-list so a foreign-runtime run can be read against a llama.cpp one — see **Foreign runtimes** below. |
| `pull <spec> [--name N] [--dry-run] [--model-loc DIR] [--license-url URL]` | Resolve revision, download (or adopt) quant-matching files, snapshot license + provenance, register. Shows a per-shard progress line on stderr (bytes, percent, rate, ETA) and resumes a partial shard from its `.part` with an HTTP `Range` request, size-verified before it is renamed into place. Idempotent: same spec+revision is a verified no-op; a NEW revision downloads but never repoints (that is `update`'s gated job). |
| `list` | Table: active marker, name, quant, size on disk, revision. |
| `use <name>` | Set the active model. Never auto-restarts — prints the restart hint. |
| `rm <name> [--yes]` | Remove a model and its files. Confirmation required; refuses the active or currently running model. |
| `show [name]` | Fully resolved server invocation + license provenance — zero mystery about what will run. |
| `doctor` | Six checks (below) — five probe the server, one compares configuration. Skipped is reported as SKIP, never PASS. |
| `setup [--dry-run]` | Engine clone/pull + cmake Metal build; creates `models/`/`logs/`; wired-limit verification (see Installation). Idempotent. |
| `update --engine\|--model\|--all [--dry-run]` | Engine: fetch the pinned `[engine] git_ref` (or git pull), rebuild, verify the built `llama-server` runs, report old→new commit. Model: re-resolve the active repo; new revisions land in a new `@rev` dir, license is diffed and **any change stops for explicit confirmation (STOP-4)** before an atomic registry repoint. Old revisions are never auto-deleted. |
| `env` | Stdout-only `ANTHROPIC_*` exports; diagnostics to stderr; safe for `eval "$(chekov env)"`. |
| `integrate hermes [--yes]` | Surgical merge into `~/.hermes/config.yaml` (details below). |
| `integrate claude` | Generate `bin/cclocal`; global Claude settings untouched. |
| `launch <agent> [--model N] [--print] [--proxy-only [--port N]] [-- args]` | Start the agent wired to the local model: proxy in-thread, generated config dir, agent as a child (auto-starts the server if it isn't running). `--print` emits the command instead of running it. `--proxy-only` runs just the foreground protocol translator (Anthropic `/v1/messages` → the server's OpenAI endpoint) on `--port` (default 8787), with no child and no generated settings — for hand-wiring a different client. |
| `completions <shell>` | Emit shell completions for `bash`, `elvish`, `fish`, `powershell` or `zsh` on stdout — what `make install` runs to write `shell/_chekov`. |

**Codebase mode** (`capability bench --codebase <PATH>`) turns the user's own
repository into a graded infill benchmark: 24 deterministic Rust tasks — 12
same-file `in_file` spans, 6 `function_body` bodies, 6 `cross_file_first`
call sites — are sampled from `HEAD` (`[bench] codebase_tasks` in
`config.toml`), masked out, and run through `/infill`. It requires a clean tree and runs in a
detached worktree, so the benchmark never touches uncommitted changes or the
branch you're on — with work in progress, bench a clone rather than the copy
you are editing. Six of the 24 tasks are `cross_file_first`: the mask is the
first use in a file of a symbol declared in exactly one other file, and each
is crossed twice — once with nothing but its own file, once with the
defining file sent as llama.cpp's `input_extra` (capped at 32 KiB, windowed
on the declaration line when the file is larger, and the row records which).
The pairing is **textual, not resolved**: chekov is not a compiler, so it
matches a declaration name against a call shape, and — since 2026-08-30 —
requires the calling file to name the defining file's module, in a `use`
statement or before a `::`. Without that second condition a bare `x.next()`
matched whichever file happened to declare `fn next`. So the `context lift`
measures what the defining file buys on tasks where the calling file already
imports it; it is not a claim that the symbol is unrecoverable otherwise.
Tiers 1–4 score the first `gold_lines` lines of each fill: `n_predict` is
generous, and a model that answers a one-line span and then keeps writing
should be graded on the answer, not on the token budget. Tier 5 reads the
whole prediction. A name declared in two or more files is ambiguous and
never masked, a name whose defining module the file never mentions is not a
candidate, and the shortfall line counts both. Tiers 6–7 (compile gate, covering test) run only under
`--allow-exec`. Because the task set now includes the new tier's ids, its
hash — and so `corpus_id` changed: runs recorded before this are not
comparable with runs after it, and `compare` refuses them by that field.
Masks are boundary-scanned, not AST-derived, and the report always says so. A file with
inline unit tests is kept and its `#[cfg(test)]` items are cut out before
anything is sampled — the header's `tests elided: L lines in F files` says what
that came to, so the model is never offered a test module as its answer. chekov
sends the whole file and grades over the whole file, but llama.cpp's `/infill`
windows the prompt at its batch size (about ¾·`n_batch` tokens of prefix and
¼·`n_batch` of suffix), so a long file reaches the model only in part — the
report's `engine window ≤ n_batch` says as much. `--dry-run` still creates and
removes a detached worktree in the target repository: the task set is sampled
from `HEAD` before anything is printed. A model without FIM tokens reports as
N/A, never as a zero — an unsupported capability is not a failing score.

**`--allow-exec`** turns on tiers 6 and 7. Tier 6 splices the fill into the
worktree's copy of the file, runs `cargo check --message-format=json
--offline`, and passes when the JSON stream carries no `error` diagnostic
anywhere in the workspace — a fill that breaks a caller in another file fails,
which is the point of the cross-file tier. Tier 7 then runs the repository's
own tests for the masked symbol: the enclosing function's name (plus the
cross-file symbol, when there is one), the nearest `Cargo.toml` above the file
for the crate, up to five `#[test]` functions in that crate whose bodies name
the symbol as a whole word — `tests/*.rs` included — and `cargo test -p <crate>
--offline -- <t> --exact` for each. Tier 7 passes only when every candidate
passes. **This runs the repository's code.** `cargo check` and `cargo test`
execute its `build.rs` scripts, its proc-macros and its tests — the same trust
as building the repository yourself. chekov bounds it and does not sandbox it:
the detached worktree is the only place written (your checkout is never
touched), one `cargo fetch` before the loop is the only networked step and
every invocation after it carries `--offline`, `CARGO_TARGET_DIR` points at
`eval/.scratch/target-<head12>` so nothing lands in the repository's own
`target/`, each check gets 120 seconds and each test run 300 with the whole
process group killed at the deadline, and every crossing is reverted with `git
checkout --` and the bytes compared before the next one starts — a worktree
that will not restore stops the run rather than measuring against a file
nobody can vouch for. Nothing here is a silent zero: a missing toolchain, an
offline registry, a timeout, a span outside every function, a crate with no
covering test are each a counted, printed reason, and the report's `compile`
mean is taken over crossings with a verdict only. Only Rust is implemented;
the module is shaped for `tsc --noEmit` and `python -m py_compile` behind the
same gate. Without the flag the ladder stops at tier 5 and the trailer reads
`tiers 6-7 skipped: --allow-exec not given`. Because the stamp records
`allow_exec`, `cargo_version` and `exec_target`, `compare` refuses across a run
that executed and one that did not — they are different environments.

**Foreign runtimes** (`capability bench NAME --runtime <name>@<version>
[--upstream <url>] [--served-model <id>]`) let an OpenAI-compatible server you
already started — MTPLX, MLX, anything on the same wire — stand in as the
bench subject.
`--runtime` is `UseRunning`-only for the subject: chekov never launches,
installs, or tears down a foreign server, and refuses before any measurement
if the named model isn't already serving (`--judge` is exempt — it still
launches chekov's own local llama.cpp judge exactly as today, though
`--runtime` together with `--judge` is refused by the existing memory-budget
gate, since a foreign server chekov did not launch never comes down for the
judge to load beside). Readiness is one plain `GET /v1/models`; the served
ids are **printed, never asserted** — chekov cannot know how a foreign server
names its own weights. The request wire's OpenAI `model` field is addressed
by what the server actually serves, never by chekov's own registry name:
`--served-model <id>` names it explicitly; absent the flag, a single served
id is used automatically, and a server listing zero or several is a refusal
(`RuntimeServedModelRequired`) naming the count rather than guessing. This
matters because llama.cpp ignores the `model` field but mlx-lm **routes** on
it, 404ing trying to download chekov's registry name from Hugging Face — a
live finding serving under the registry name works around, `--served-model`
fixes properly. The registry name still names everything else: the run
directory, the stamp's weights identity, and the report header. A
thinking-default model must be served with reasoning disabled (mlx-lm:
`--chat-template-args '{"enable_thinking": false}'`) or the codebase suite's
chat-FIM fills burn the gold-bounded budget on reasoning and fail loudly as
`chat fill has no text content` — a second live finding, not yet automated.
Launch flags chekov cannot observe are stamped with
fixed sentinels (`ctx`/`n_parallel` `0`; the six flag fields `"unmanaged"`,
a third spelling distinct from `"engine-default"`) rather than invented, and
the stamp's new `runtime` field (`llama.cpp` unless declared — every run on
disk already reads that way) sits ahead of `engine_build_commit`, so a
cross-runtime pair mismatches there first. Codebase mode's FIM crossing rides
`/infill` for llama.cpp and a deterministic chat-completions instruction for
a runtime with none; the report names which (`fim transport: /infill` or
`fim transport: chat`). `capability compare --cross-runtime` permits exactly
`runtime`, `engine_build_commit`, the eight unmanaged fields and
`prompt_set_hash` to differ, opens with a loud banner ending "this measures
the runtimes, not the model.", and still refuses on everything else — plain
`compare` refuses a cross-runtime pair on `runtime` like any other mismatch.
Throughput and codebase chat-FIM crossings on a foreign run no longer need
llama.cpp's non-standard `timings` object: they are timed by chekov's own
wall clock over the streamed response instead. OpenAI-shaped `usage` token
counts (`prompt_tokens`, `completion_tokens`) combine with two measured
windows — request written → first SSE data frame, first data frame → stream
end — to derive the same `prompt_per_second`/`predicted_per_second` shape
the report already prints (decode divides by n−1 tokens, since the first
token lands at the first-data mark; `cache_n` is recorded `0`, unknowable
through a foreign server). This is honest client-wall-clock timing: it
includes wire and translator overhead (microseconds on localhost, negligible
against token times), and the first-data mark only approximates end-of-prefill
because these servers stream tokens as they are generated — the report says
so. A reply chekov cannot derive a timing from (no `usage` frame, fewer than
2 completion tokens, a zero-length window) fails loudly per probe, naming the
declared runtime and exactly what was missing. The stamp's `timing_source`
field (`server-reported` by default — every run already on disk reads
unaffected; `chekov-streamed` on a foreign run) drives one report line,
`timing source: chekov-streamed (client wall-clock over SSE; includes wire
overhead)`, printed only when it isn't `server-reported`; `--cross-runtime`
now also permits `timing_source` to differ. The codebase mode's
chat-completions FIM fallback rides the same timed crossing, so foreign
codebase rows carry real timings too; the llama.cpp `/infill` arm and its
timing path are untouched. Agentic and fixture suites are not yet on the
timed path — their foreign-run row failures name the runtime and the exact
reason instead of prescribing an engine rebuild, but riding the same timed
mechanism is a recorded follow-up. Live-verified against mlx-lm 0.31.3
serving Ornith-1.5-35B-A3B-MLX on this machine (see IDEAS.md); foreign
agentic/fixture timing remains a follow-up.

### The six doctor checks

1. **OpenAI door** — `POST /v1/chat/completions` returns content
2. **Anthropic door** — `POST /v1/messages` returns content
3. **Think-tag retention** — response keeps `<think>` (only when the model's
   flags include `--reasoning-format none`; otherwise SKIP with a note)
4. **NaN canary** — ~1,500-token code generation; fails on ≥30 identical
   consecutive tokens or U+FFFD density over threshold (guards the known
   GGUF `blk.61` corruption class)
5. **Context floor** — effective ctx ≥ 65536 when `hermes_ok = true`
   (hard fail); advisory SKIP otherwise. Compares `models.toml` to
   `config.toml` only — the one row that can pass with the server down
6. **Context loaded** — the server's `/props` per-slot `n_ctx` equals the
   effective `ctx_size`; the same assertion the bench makes before it records
   a run. A mismatch names both numbers; an unreachable server is a FAIL

## Runbook

### Daily driving

```sh
chekov status                 # is it up? which model/revision? wired limit?
chekov run                    # start in the background (refuses if something is off)
chekov doctor                 # full health pass — run after any change
chekov stop                   # clean shutdown
tail -f logs/llama-server.log # watch the server
```

Model load time: ~2 minutes for a ~158 GiB model from a fast external SSD.
`chekov run` returns immediately; poll `curl -s localhost:8080/health`
(503 while loading, 200 when ready) or just run `chekov doctor`.

### Finding a model

A full model-selection guide (quants, GGUF, sampling params, picking a model
for your RAM) lives in [docs/HOWTOS.md](docs/HOWTOS.md). The short version:
chekov runs any GGUF repo on Hugging Face. A workable way to choose:

1. **Start from a quantizer you trust.** [unsloth](https://huggingface.co/unsloth)
   and [bartowski](https://huggingface.co/bartowski) publish GGUF conversions of
   most notable open-weight releases within days, with a quant table on every
   model card; filter the Hub by the `gguf` library tag to find others.
2. **Read the model card's quant table.** Each row is a tag (`Q4_K_M`,
   `UD-Q5_K_XL`, `Q8_0`, …) with a file size. The tag is what goes after the
   colon in the pull spec.
3. **Size it against your memory.** Rule of thumb: `weights + KV cache + ~3 GiB`
   must fit under the GPU budget `chekov capability` prints (`run` checks
   exactly this, and `[limits] wired_limit_mb` can pin a floor). KV cache at
   q8_0 is roughly `ctx × cached_layers × kv_heads × head_dim × 2 × 1.0625
   bytes` — q8_0 is 34 bytes per 32 elements, not one byte, and
   `cached_layers` is **not** the model's layer count on modern architectures:
   MoE and sliding-window models cache only a fraction of their blocks, so
   using `block_count` overestimates by 4-5×. When in doubt, launch once and
   read `llama_kv_cache: size = …` from the server log — measured beats
   predicted. For a 100k context on a mid-size model budget 10–20 GiB. Pick the
   largest quant
   that leaves that headroom — `chekov run` refuses to start rather than let
   macOS page a model, so an over-ambitious pick fails loudly, not slowly.
4. **Note the vendor's sampling advice.** Model cards usually list
   recommended `temperature` / `top_p` / `top_k` and whether the model emits
   `<think>` blocks; those become the model's `extra_flags`.

Not sure which tags a repo offers? Pull without one and chekov lists them:

```sh
chekov pull unsloth/Qwen3.8-27B-GGUF
# error: no quant tag given for unsloth/Qwen3.8-27B-GGUF and there is no silent default.
# Available tags, UD-Q4_K_XL, UD-Q5_K_XL, UD-Q6_K_XL, Q8_0, …
#
# re-run: chekov pull unsloth/Qwen3.8-27B-GGUF:<QUANT>
```

### Adding a model

```sh
chekov pull unsloth/Qwen3.8-27B-GGUF:UD-Q6_K_XL          # download to <root>/models/
chekov pull unsloth/Qwen3.8-27B-GGUF:UD-Q6_K_XL \
    --model-loc /Volumes/external/models                  # …or onto an external volume
chekov show qwen3.8-27b                                   # the exact llama-server invocation
```

`pull` resolves the repo's current revision, downloads only the files for
that quant, snapshots the license, and registers the model under a short name
(`unsloth/Qwen3.8-27B-GGUF` → `qwen3.8-27b`; override with `--name`). Then
tune its entry in `models.toml` — this is the one place where a file edit is
normal:

```toml
[models."qwen3.8-27b"]
# …fields written by pull…
ctx_size = 131072                      # override [defaults].ctx_size for this model
hermes_ok = true                       # enforce the 65536 ctx floor in `doctor`
extra_flags = ["--reasoning-format", "none",   # keep <think> blocks in the output
               "--temp", "0.7", "--top-p", "0.8", "--top-k", "20"]
```

`extra_flags` are appended after `[defaults].flags`, never replacing them.

`-np 1` pins llama-server to a single KV slot. Without it, `--parallel` is auto
and `--ctx-size` becomes a pool shared across slots, so concurrent agent
requests exhaust it mid-generation with "Context size has been exceeded" — and
`chekov status` still reports the full number. Keep the pin unless you know you
want the shared pool; the trade is that background agent traffic serialises
behind the foreground turn.
Then activate and verify:

```sh
chekov use qwen3.8-27b
chekov restart            # or `chekov run` if nothing is running
chekov doctor             # both doors, think-tags, NaN canary, ctx floor + loaded
```

Pin a specific revision with `org/repo:QUANT@<sha>`; use `--dry-run` to see
what would be downloaded; `--license-url` points the license snapshot at a
non-standard location when the repo keeps it elsewhere.

`--dry-run` prints the shard list with each file's byte size and the directory
they would land in, and registers nothing. A download **resumes a partial
shard**: each in-flight shard is written to a `.part` sibling, and the next
`chekov pull` asks the hub for the rest of it with an HTTP `Range` request
instead of starting the file again — which matters when a single shard is
40 GB. The resumed bytes are checked before they are appended (the server has
to answer `206` at exactly the offset already on disk, for a file of the size
the API published) and the finished `.part` is checked against that size again
before it is renamed into place: a short file is never renamed, and its `.part`
is kept for the next run. A `.part` longer than the file can be is discarded
rather than appended to. While a shard is in flight, a progress line — shard
number, bytes, percent, rate, ETA — is written to **stderr**, so `chekov pull >
log` keeps its one-line-per-shard stdout unchanged.

**Which repo layouts `pull` reads.** The quant tag is matched against three
shapes, and a repo only has to use one of them:

- a folder per quant named by the tag — unsloth's `UD-Q5_K_XL/…`;
- a folder per quant named after the model, with the tag in the shard's own
  filename — bartowski's
  `Model-IQ3_M/Model-IQ3_M-00001-of-00005.gguf`;
- flat files in the repo root — `Model-Q8_0.gguf`.

The tag may be dot-separated (`Model.Q4_K_M.gguf`, the mradermacher style) and
may be lowercase (`qwen2.5-0.5b-instruct-q4_k_m.gguf`, the `Qwen/*-GGUF`
style): a spec matches a tag case-insensitively, and the registry records the
repo's own spelling, because that spelling is also the download path. If one
repo carries two spellings of the same tag, both are listed and an inexact
spec is refused naming them — give the exact spelling. A repo that still
reports "this repo exposes no .gguf files" has files whose names carry no
`Q…`/`IQ…`/`BF16`/`F16`/`F32` token at all; check its file list.

### Swapping models

Every registered model is one `use` away:

```sh
chekov list               # what is registered, sizes, which is active
chekov use minimax-m2.7
chekov restart
```

To see available quant variants for any repo, pull without a quant suffix:

```sh
chekov pull unsloth/DeepSeek-V3-GGUF   # errors with the available tags
```

No file edits needed. `chekov show <name>` prints the exact invocation.

### Updating

```sh
chekov update --engine            # llama.cpp: pull + rebuild, old→new commit
chekov update --model             # active model: new revision + license gate
chekov update --all --dry-run     # preview both
```

`update --model` will print a license diff and stop for an explicit `y` if
the license text changed between revisions — vendors have re-licensed
post-release before; this gate is deliberate. Old revision dirs stay on disk
until you `chekov rm` them.

**Pinning the engine.** Weights are revision-pinned; without a pin the binary
that runs them is whatever upstream HEAD was on the day of `setup` /
`update --engine`. Set `[engine] git_ref` in `config.toml` to a branch, tag or
commit and both commands `git fetch origin <ref>` + `git checkout --detach
FETCH_HEAD` instead of fast-forwarding, and `update --engine` reports
`engine: <old> → <new> (pinned to <ref>)`. Absent, nothing changes. A ref git
would read as an option (`-…`) or that splits into several arguments is
refused at config load, naming the key.

Every engine build ends by running the binary it just produced
(`llama-server --version`), so a llama.cpp change that breaks the build's
output fails there as `EngineStepFailed` naming the step, rather than later as
a failed `run`. There is no auto-rollback: `logs/chekov.engine` names the
commit to go back to.

**A diverged engine checkout.** If you have ever cherry-picked a fix into
`llama.cpp/` by hand, that checkout carries a local commit and is no longer a
descendant of `origin/master`, so the unpinned path — whose step is
`git pull --ff-only` — fails at "update llama.cpp checkout" with git's
`Not possible to fast-forward, aborting`. Nothing was rebuilt and the engine
you have keeps working; only the update stopped. Either set `[engine] git_ref`
to the ref you actually want, or reconcile and rebuild by hand with the same
lines chekov itself runs:

```sh
git -C llama.cpp fetch origin && git -C llama.cpp rebase origin/master
cmake -S llama.cpp -B llama.cpp/build -DGGML_METAL=ON -DCMAKE_BUILD_TYPE=Release
cmake --build llama.cpp/build --config Release --target llama-server -j
llama.cpp/build/bin/llama-server --version     # the verify step
```

**Updating chekov itself.** `make` and `make setup` build into `target/`; they
do **not** refresh the binary on your PATH. `make install` does — it re-runs
`cargo install --path .` and regenerates the completions. After pulling new
commits, run `make install` before blaming a missing flag on the tool: a
days-old `~/.cargo/bin/chekov` is the likelier explanation.

### Troubleshooting

| Symptom | Meaning / fix |
|---|---|
| `port 8080 is already in use` | `chekov status`; if it's a chekov server, `chekov stop`/`restart`; otherwise free the port or change `[server] port` in `config.toml`. |
| `model 'X' needs about N MiB at ctx C … but this Mac's GPU budget is B MiB` | The model does not fit this machine at that context: pull a smaller quant, lower its `ctx_size` in `models.toml`, or `chekov capability recommend` to see what fits. No sysctl changes this. |
| `wired limit is X MB but Y MB is required` | You configured `[limits] wired_limit_mb`: run the printed `sudo sysctl iogpu.wired_limit_mb=Y`, then retry. Reboots reset it. |
| `stale pidfile … cleaned` on `stop` | The server died earlier (check the log tail). Just `chekov run` again. |
| Doctor: NaN canary FAIL | Matches the known GGUF corruption class — re-pull the shards (`chekov pull <spec>`, size-verified) and re-run doctor. |
| Doctor: think-tag FAIL | The model's `extra_flags` lost `--reasoning-format none`, or the template ate the tags — check `chekov show`. |
| First Claude Code call is slow (~5–6 min) | Expected: Claude Code's initial request carries a ~60k-token system+tools prompt; MiniMax prompt-processes at ~180 tok/s. The server's prompt cache makes subsequent calls fast. |
| Registry corrupt | The error names the file; restore from a backup or delete `models.toml` and re-`pull` (weights are untouched). |
| `Not possible to fast-forward` on `setup` / `update --engine` | The `llama.cpp/` checkout has diverged from `origin/master` (a hand-applied commit), and the step is a `git pull --ff-only`. The built engine is untouched. Pin `[engine] git_ref`, or rebase and rebuild by hand — see [Updating](#updating). |
| `quant tag 'X' not found … (none — this repo exposes no .gguf files)` | Either the installed binary predates the folder-per-quant fix (`make install`), or the repo's filenames carry no `Q…`/`IQ…`/`BF16`/`F16`/`F32` token in any spelling (dot- and dash-separated, upper- and lowercase are all read). Check the repo's file list before assuming the tag is wrong. |
| `WorkingTreeDirty` from `capability bench --codebase` | Codebase mode refuses a dirty tree so the task set is exactly what `HEAD` says. Commit, stash, or bench a clean clone of the repository. |
| `ExecWorktreeDirty` from `capability bench --codebase --allow-exec` | A `git checkout --` did not restore the file tier 6 spliced. The run stopped rather than measure the next crossing against a file it cannot vouch for. Inspect the worktree the message names, delete it (`git worktree remove --force <path>`, then `git worktree prune`), and resume with `--resume <RUN>`: every row up to that crossing is intact. |
| `codebase N/A — infill unsupported by this model` | The model's GGUF carries no FIM tokens, so `/infill` has nothing to fill. That is a missing capability, not a failing score — it is never reported as a zero. |
| `tune: the baseline for 'X' could not be measured` | The model's own current flags did not survive a probe (the server never became ready, the probe returned no timings, or too few samples survived warmup) — there is nothing to compare candidates against. Run `chekov run X` and `chekov doctor` first; fix whatever they surface, then retry `chekov tune X`. |

## Integrations

### Hermes (`chekov integrate hermes`)

Performs a **surgical merge** of `~/.hermes/config.yaml` — a live Hermes
config carries providers, MCP servers, toolsets, and plugin state that must
survive. chekov changes exactly two things and nothing else:

1. the top-level `model:` block → `provider: chekov`, local base_url/api_key,
   `default:` = active model alias, `context_length:` = effective ctx
2. a `chekov:` entry under `providers:` (inserted or replaced)

Guard rails: refuses if `~/.hermes` doesn't exist (STOP-3 — chekov never
creates another tool's config tree); switching away from a live non-chekov
provider requires confirmation (`--yes` to pre-approve); the previous file is
backed up to `config.yaml.bak-<UTC>` first; a second run is a clean no-op.
Hard error if the active model's effective ctx is below 65536 while
`hermes_ok = true`.

Verify: `hermes -z "hello"` (one-shot), or `hermes model` to see/switch
providers. To revert: `hermes model` back to your old provider, or restore
the `.bak-<UTC>` file.

### Claude Code (`chekov integrate claude`)

Generates `bin/cclocal` (also reachable as the `cclocal` alias):

```sh
cclocal                      # interactive Claude Code on the local model
cclocal -p "quick question"  # headless one-shot
```

It evals `chekov env` and execs `claude` — cloud Claude Code stays the
default; nothing global is modified. If `chekov env` fails (not installed,
no active model), cclocal **aborts loudly instead of silently falling back
to the cloud** (that failure mode actually happened; it's regression-tested).

`chekov env` exports `ANTHROPIC_BASE_URL`, `ANTHROPIC_AUTH_TOKEN`, and the
three `ANTHROPIC_DEFAULT_{OPUS,SONNET,HAIKU}_MODEL` variables pointing at the
active alias, so any Anthropic-SDK tool can be pointed locally with
`eval "$(chekov env)"`.

### Claude Code (`chekov launch claude`)

```sh
chekov launch claude                   # interactive, on the local model
chekov launch claude -- -p "question"  # args after -- go to claude
chekov launch claude --print           # emit the command, run it yourself
chekov launch claude --proxy-only      # just the translator on :8787 (no child)
```

Starts the model server if it is down, binds the translator on an ephemeral
loopback port, and runs `claude` as a child — so the proxy exits with the
session rather than lingering on a port.

**Why a config dir and not environment variables.** Claude Code writes every
`env` entry from its settings file into the process environment at startup,
replacing what the shell exported. A launcher that only sets variables is a
no-op for anyone who pins `ANTHROPIC_MODEL` in `~/.claude/settings.json` —
requests silently go out under the pinned model and fail. `chekov launch`
therefore generates `<root>/agents/claude/settings.json` and points Claude
Code at it with `CLAUDE_CONFIG_DIR`. Your real settings are never touched.

The generated settings carry your `mcpServers`, `hooks`, `enabledPlugins`,
`permissions`, and `extraKnownMarketplaces` forward, so a local session keeps
the tools you expect; only the `env` block is chekov's. Plugins installed from
a **local-directory marketplace** are mirrored into the session's
`plugins/` tree (symlink + `installed_plugins.json` / `known_marketplaces.json`
entries) so `enabledPlugins` resolves without a "marketplace not found"
warning; git-backed marketplaces need nothing special. `ANTHROPIC_CUSTOM_MODEL_OPTION` is what makes a
non-Anthropic id such as `minimax-m2.7` selectable in `/model` — it is the one
id accepted without a validation probe. Gateway discovery is deliberately not
used: it filters ids to those containing `claude`, which would mean renaming
the model to satisfy a substring check and forfeiting the honest
`CLAUDE_CODE_MAX_CONTEXT_TOKENS` declaration.

Claude Code still logs `[claude-code:unrecognized_model]` once at startup from
its own session-title call. Cosmetic — the request that matters is served.

## Pull-spec grammar

```
chekov pull org/repo                 # no quant → error listing available tags (no silent default)
chekov pull org/repo:QUANT           # e.g. unsloth/MiniMax-M2.7-GGUF:UD-Q5_K_XL
chekov pull org/repo:QUANT@rev       # explicit revision pin
chekov pull org/repo@rev
chekov pull https://huggingface.co/org/repo        # normalized to org/repo
```

Short names derive from the repo tail: `-GGUF` stripped, lowercased
(`unsloth/MiniMax-M2.7-GGUF` → `minimax-m2.7`). Override with `--name`.
Quant tags are matched by a single source of truth — subdir-style
`UD-Q5_K_XL/…`, a model-named subdir carrying the tag in the shard filename
(`Model-IQ3_M/Model-IQ3_M-00001-of-00005.gguf`), flat `…-Q8_0.gguf`, and
dot-separated `Model.Q4_K_M.gguf` all work — so `Q5_K_XL` can never
accidentally select `UD-Q5_K_XL` files. Matching is case-insensitive
(`Q4_K_M` selects a repo's `q4_k_m`) and the repo's spelling is what gets
recorded; a repo with two spellings of one tag refuses an inexact spec.

## Registry: flags concatenate, never replace

`models.toml` holds `[defaults]` (ctx_size, flags) and per-model tables. A
model's effective flags are **`defaults.flags` followed by its
`extra_flags`** — extras append, they never replace. `ctx_size` is the one
scalar override. Inspect any resolution with `chekov show <name>`.

```toml
active = "minimax-m2.7"          # top-level keys must precede [tables]

[defaults]
ctx_size = 98304
flags = ["--jinja", "--flash-attn", "on",
         "--cache-type-k", "q8_0", "--cache-type-v", "q8_0",
         "-np", "1"]        # one KV slot: see below

[models."minimax-m2.7"]
repo = "unsloth/MiniMax-M2.7-GGUF"
quant = "UD-Q5_K_XL"
revision = "d2a05ccf69491b03db0cc40b335aec14bdaf7198"
path = "/Volumes/external/models/minimax-m2.7@d2a05ccf6949"   # absolute = --model-loc
first_shard = "UD-Q5_K_XL/MiniMax-M2.7-UD-Q5_K_XL-00001-of-00005.gguf"
hermes_ok = true
extra_flags = ["--reasoning-format", "none",
               "--temp", "1.0", "--top-p", "0.95", "--top-k", "40"]
```

An entry that `capability bench --judge <NAME>` should serve needs one more
field, added by hand — `pull` never writes it, and `role` gates nothing else,
though `chekov list` marks the entry that carries it in its `ROLE` column:
`role = "judge"` on that `[models.<name>]` table. Any other value is refused
at registry load, naming the one accepted value: `role = "candidate" is not a
role chekov knows; the one accepted value is "judge"`.

## License snapshots — why

Each pull writes `LICENSE.snapshot` and `LICENSE.provenance` (repo, revision,
source URL, UTC) beside the weights. Vendors have changed license text after
release; `update --model` diffs the new revision's license against the
snapshot and **stops on any change** (STOP-4), so you can never be silently
migrated onto different terms. A repo with no license file is recorded as
such — honestly, not skipped.

## Configuration (`config.toml`, all optional)

```toml
[server]
host = "127.0.0.1"        # default
port = 8080               # default
api_key = "chekov-local"  # default; passed to llama-server --api-key

[limits]
# wired_limit_mb = 187000 # optional floor: `run` refuses below it. Absent (the
                          # default), the model's own footprint is the requirement
hermes_ctx_floor = 65536  # hard floor when a model is hermes_ok

[doctor]
canary_max_tokens = 1500
degenerate_run_len = 30
replacement_char_max_pct = 5

[engine]
# git_ref = "b7000"       # pin the engine to a branch, tag, or commit;
                          # absent = upstream HEAD on the day of setup/update

[bench]                          # `chekov capability bench`
depths = [1024, 4096, 16384]     # prompt depths swept, in approximate tokens
repetitions = 5                  # per depth; the first is dropped as warmup
max_tokens = 128                 # decode length per probe
significance_pct = 5             # median delta below this: no difference claimed
ready_max_polls = 600            # readiness budget: polls × interval
ready_interval_ms = 500
seed = 42                        # sampling seed pinned onto every probe
release_pct = 80                 # teardown waits for this % of the budget to free
release_max_polls = 60           # release budget: polls × interval
release_interval_ms = 500
codebase_tasks = 24              # `--codebase` tasks per run (⅔ in_file, ⅓ function_body)
judge_max_tokens = 512           # --judge reply budget; 2x the largest reply measured in the 2026-08-30 probe
judge_min_consistency_pct = 70   # swap-agreement floor below which the equiv column is voided
judge_reasoning_effort = "low"   # none|low|medium|high, forwarded to the judge's wire only

[tune]                                    # `chekov tune`
depth = 4096                              # probe prompt depth in tokens
flash_attn = ["on", "off"]                # stage fa
cache_types = ["q8_0", "f16"]             # stage kv (applied to K and V together)
batch_sizes = [512, 1024, 2048, 4096]     # stage batch
ubatch_sizes = [256, 512, 1024, 2048]     # stage ubatch (≤ the incumbent batch)
```

`[limits] hermes_ctx_floor` is the hard context floor `doctor` enforces for a
model marked `hermes_ok`; `[doctor] replacement_char_max_pct` is the U+FFFD
density above which the NaN canary fails. `[bench] judge_max_tokens` bounds a
`--judge` reply (default 512, twice the largest completion the 2026-08-30
probe measured); `judge_min_consistency_pct` (default 70) is the swap-
agreement floor below which the report's `equiv` column is voided rather than
trusted; `judge_reasoning_effort` (default `low`) is forwarded to the judge's
wire only — gpt-oss needs it to bound its thinking, Gemma's template ignores
it. `[tune]`'s five keys are `chekov tune`'s own probe depth and its four
stages' candidate lists; `repetitions`, `max_tokens` and `significance_pct`
come from `[bench]` — one definition of "how many samples" and "what is
significant" for every measurement chekov makes.

Unknown keys are rejected loudly (deny_unknown_fields), never ignored.
`config.example.toml` is a commented starting point; `config.toml` itself is
gitignored because the numbers are machine-specific. `CHEKOV_HOME` overrides
the root directory (default `~/.chekov`; a source install's shell shim sets
it to the clone).

## Layout

```
config.example.toml  # commented template for the machine tunables (above)
config.toml          # your copy of it (gitignored)
models.toml          # the registry — managed by pull/use/rm/update (gitignored)
models/<name>@<rev12>/     # weights + REVISION + LICENSE.snapshot/.provenance
                           # (or an absolute --model-loc dir)
logs/                # chekov.pid, chekov.model, llama-server.log
logs/chekov.engine   # the commit the engine was built from — the rollback pointer
llama.cpp/           # engine checkout + Metal build (managed by setup)
eval/<run-id>/       # one stored bench run: stamp.json + results.jsonl
eval/.scratch/       # transient `--codebase` worktree; hidden from every enumerator
reports/             # default destination for `capability graph --svg`
agents/<agent>/      # generated agent settings for `chekov launch` (gitignored)
bin/cclocal          # generated by `chekov integrate claude`
shell/chekov.zsh     # PATH + cclocal alias + completions (sourced from ~/.zshrc)
shell/_chekov        # generated zsh completions (make install)
```

## Development

```sh
make test   # cargo test — 150+ tests, all offline against fakes/fixtures
make deny   # cargo deny check — licenses, advisories, duplicate crates
make lint   # cargo fmt --check && cargo clippy --all-targets -- -D warnings
```

- clippy runs pedantic+nursery under `-D warnings`; `clippy.toml` encodes the
  house limits (≤40-line functions, ≤3 args, bounded nesting)
- HTTP is a trait object (`HttpClient`) — tests inject canned responses; no
  test ever touches the network or a real llama.cpp
- TDD with committed-red history per module (`git log --oneline` shows the
  red→green pairs); repo-specific standards overrides live in `AGENTS.md`
- `deny.toml` is authored for CI supply-chain checks (`cargo deny`); CI
  (`.github/workflows/ci.yml`) runs fmt + clippy + tests on macOS and
  `cargo deny` on every push and PR
- The pre-push gate is `pushkin floor` — fmt, clippy pedantic under
  `-D warnings`, and the test suite in one command; the `make` targets above
  are its individual halves
- Changes are tracked in [CHANGELOG.md](CHANGELOG.md); open ideas and their
  status live in [IDEAS.md](IDEAS.md)

### Cutting a release

1. Bump `version` in `Cargo.toml`, move the `[Unreleased]` notes in
   `CHANGELOG.md` under a new dated heading, commit on `main`.
2. `git tag -a vX.Y.Z -m "vX.Y.Z" && git push origin vX.Y.Z`.
3. `.github/workflows/release.yml` verifies the tag matches `Cargo.toml`,
   re-runs lint + tests, attaches an arm64 tarball to a GitHub Release, and
   publishes `chekov-mac` to crates.io (needs the `CARGO_REGISTRY_TOKEN`
   repository secret; the job skips with a warning when it is absent).

## License

[MIT](LICENSE). Model weights pulled by chekov carry their own licenses —
each pull snapshots the license text beside the weights (see above).
