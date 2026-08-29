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

`setup` ends by verifying `iogpu.wired_limit_mb`. A sysctl value of `0`
means "macOS system default" and is resolved as **75% of RAM** (e.g. 192 GiB
on a 256 GB machine) — it is not treated as zero. If the effective limit is below
`[limits] wired_limit_mb` in `config.toml`, setup prints the exact
`sudo sysctl iogpu.wired_limit_mb=<N>` command and marks itself incomplete;
run it yourself and re-run `make setup` to verify. **chekov never executes
sudo.**

## Quickstart

```sh
chekov pull unsloth/MiniMax-M2.7-GGUF:UD-Q5_K_XL
chekov use minimax-m2.7
chekov run                # starts in the background by default
chekov doctor            # five health checks; non-zero exit on any failure
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
| `stop` | SIGTERM via pidfile, 20 s grace, SIGKILL escalation with a warning. Detects and cleans stale pidfiles. |
| `restart [name]` | Stop (if running) then start in the background; swaps models in one motion. |
| `status` | running/pid, model, revision, port, ctx, uptime, wired-limit actual (with system-default annotation) vs required, log tail path. |
| `pull <spec> [--name N] [--dry-run] [--model-loc DIR] [--license-url URL]` | Resolve revision, download (or adopt) quant-matching files, snapshot license + provenance, register. Idempotent: same spec+revision is a verified no-op; a NEW revision downloads but never repoints (that is `update`'s gated job). |
| `list` | Table: active marker, name, quant, size on disk, revision. |
| `use <name>` | Set the active model. Never auto-restarts — prints the restart hint. |
| `rm <name> [--yes]` | Remove a model and its files. Confirmation required; refuses the active or currently running model. |
| `show [name]` | Fully resolved server invocation + license provenance — zero mystery about what will run. |
| `doctor` | Five checks (below) — four probe the server, one compares configuration. Skipped is reported as SKIP, never PASS. |
| `capability recommend [--ctx N] [--role agent\|chat] [--refresh] [--limit N]` | Ranks the registered models for this machine. Gates first — anything that exceeds the budget, or cannot be sized, is listed with its reason rather than dropped. Then sorts: under `--role agent` a model whose chat template has no dedicated llama.cpp tool parser is **downranked with a note, not refused**; under `--role chat` the tool parser is ignored. `--refresh` is the **only** networked path — without it chekov ranks registered models only and never reaches out. |
| `capability explain [name] [--ctx N]` | Read one model's GGUF header and print its fit arithmetic line by line: block count, the MTP/interval layer ladder, padded context, cache type, KV bytes, weights on disk. Local file read; no network. |
| `capability graph [--ctx N]...` | Grid of registered models against context lengths. Each cell is two characters: the fit verdict, then whether its inputs were measured or predicted. A predicted GPU ceiling is announced in the header and changes the legend, because every verdict below it is then measured against a guess. |
| `capability [--json]` | What this Mac is and what it can hold: chip, GPU cores, performance threads, macOS, and the GPU budget **with its provenance**. The budget is read from the engine (`llama-server --list-devices`) when it is built, from `iogpu.wired_limit_mb` when set, and only otherwise from the 75%-of-RAM formula — which measures 31457 MiB low on a 256 GiB M3 Ultra, so the source is always printed. |
| `setup [--dry-run]` | Engine clone/pull + cmake Metal build; creates `models/`/`logs/`; wired-limit verification (see Installation). Idempotent. |
| `update --engine\|--model\|--all [--dry-run]` | Engine: fetch the pinned `[engine] git_ref` (or git pull), rebuild, verify the built `llama-server` runs, report old→new commit. Model: re-resolve the active repo; new revisions land in a new `@rev` dir, license is diffed and **any change stops for explicit confirmation (STOP-4)** before an atomic registry repoint. Old revisions are never auto-deleted. |
| `integrate hermes [--yes]` | Surgical merge into `~/.hermes/config.yaml` (details below). |
| `integrate claude` | Generate `bin/cclocal`; global Claude settings untouched. |
| `env` | Stdout-only `ANTHROPIC_*` exports; diagnostics to stderr; safe for `eval "$(chekov env)"`. |
| `launch <agent> [--model N] [--print] [--proxy-only [--port N]] [-- args]` | Start the agent wired to the local model: proxy in-thread, generated config dir, agent as a child (auto-starts the server if it isn't running). `--print` emits the command instead of running it. `--proxy-only` runs just the foreground protocol translator (Anthropic `/v1/messages` → the server's OpenAI endpoint) on `--port` (default 8787), with no child and no generated settings — for hand-wiring a different client. |

### The five doctor checks

1. **OpenAI door** — `POST /v1/chat/completions` returns content
2. **Anthropic door** — `POST /v1/messages` returns content
3. **Think-tag retention** — response keeps `<think>` (only when the model's
   flags include `--reasoning-format none`; otherwise SKIP with a note)
4. **NaN canary** — ~1,500-token code generation; fails on ≥30 identical
   consecutive tokens or U+FFFD density over threshold (guards the known
   GGUF `blk.61` corruption class)
5. **Context floor** — effective ctx ≥ 65536 when `hermes_ok = true`
   (hard fail); advisory SKIP otherwise

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
   must fit under `[limits] wired_limit_mb` (default: 187000 MB). KV cache at
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
chekov doctor             # both API doors, think-tags, NaN canary, ctx floor
```

Pin a specific revision with `org/repo:QUANT@<sha>`; use `--dry-run` to see
what would be downloaded; `--license-url` points the license snapshot at a
non-standard location when the repo keeps it elsewhere.

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

### Troubleshooting

| Symptom | Meaning / fix |
|---|---|
| `port 8080 is already in use` | `chekov status`; if it's a chekov server, `chekov stop`/`restart`; otherwise free the port or change `[server] port` in `config.toml`. |
| `wired limit is X MB but Y MB is required` | Run the printed `sudo sysctl iogpu.wired_limit_mb=Y`, then retry. Reboots reset it. |
| `stale pidfile … cleaned` on `stop` | The server died earlier (check the log tail). Just `chekov run` again. |
| Doctor: NaN canary FAIL | Matches the known GGUF corruption class — re-pull the shards (`chekov pull <spec>`, size-verified) and re-run doctor. |
| Doctor: think-tag FAIL | The model's `extra_flags` lost `--reasoning-format none`, or the template ate the tags — check `chekov show`. |
| First Claude Code call is slow (~5–6 min) | Expected: Claude Code's initial request carries a ~60k-token system+tools prompt; MiniMax prompt-processes at ~180 tok/s. The server's prompt cache makes subsequent calls fast. |
| Registry corrupt | The error names the file; restore from a backup or delete `models.toml` and re-`pull` (weights are untouched). |

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
Quant tags are matched by a single source of truth (subdir-style
`UD-Q5_K_XL/…` and flat `…-Q8_0.gguf` both work), so `Q5_K_XL` can never
accidentally select `UD-Q5_K_XL` files.

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
wired_limit_mb = 187000   # required GPU wired memory before `run` proceeds
hermes_ctx_floor = 65536  # hard floor when a model is hermes_ok

[doctor]
canary_max_tokens = 1500
degenerate_run_len = 30
replacement_char_max_pct = 5

[engine]
# git_ref = "b7000"       # pin the engine to a branch, tag, or commit;
                          # absent = upstream HEAD on the day of setup/update
```

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
llama.cpp/           # engine checkout + Metal build (managed by setup)
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
- Changes are tracked in [CHANGELOG.md](CHANGELOG.md)

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
