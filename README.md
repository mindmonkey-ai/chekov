# chekov

Local llama.cpp inference stack manager for Apple Silicon (built for a Mac
Studio M3 Ultra, 256 GB). One static binary owns the full lifecycle —
**pull → run → stop/restart → status → doctor → update** — with models
abstracted behind a registry so adding one is a single `pull`, ollama-style.
Integrates with zsh, Hermes Agent, and Claude Code.

- Package name: `chekov-mac` (crates.io `chekov` is taken); binary: `chekov`
- No async runtime; blocking `ureq` + `hf-hub` for downloads
- Every failure is loud and names its remediation command; nothing degrades
  silently (no auto-shrunk ctx, no model fallback, no skipped checks)

## Requirements

- macOS on Apple Silicon, Xcode CLT, `cmake` and `git` (Homebrew is fine)
- Rust stable (pinned by `rust-toolchain.toml`; rustup will honor it)
- Disk for weights (~160 GB per large model) — external volumes supported
  via `--model-loc`

## Installation

```sh
git clone <this repo> ~/personal_dev/chekov && cd ~/personal_dev/chekov

make setup      # cargo release build + clone/cmake llama.cpp with Metal
                # (builds llama-server, llama-cli, llama-gguf-split)

make install    # cargo install --path .  → chekov on PATH (~/.cargo/bin)
                # generates zsh completions (shell/_chekov)
                # appends ONE idempotent source line to ~/.zshrc

exec zsh        # pick up PATH, `cclocal` alias, tab completion
```

`setup` ends by verifying `iogpu.wired_limit_mb`. A sysctl value of `0`
means "macOS system default" and is resolved as **75% of RAM** (192 GiB on a
256 GB machine) — it is not treated as zero. If the effective limit is below
`[limits] wired_limit_mb` in `config.toml`, setup prints the exact
`sudo sysctl iogpu.wired_limit_mb=<N>` command and marks itself incomplete;
run it yourself and re-run `make setup` to verify. **chekov never executes
sudo.**

## Quickstart

```sh
chekov pull unsloth/MiniMax-M2.7-GGUF:UD-Q5_K_XL
chekov use minimax-m2.7
chekov run --daemon
chekov doctor            # five health checks; non-zero exit on any failure
```

Already have the weights on an external drive (huggingface-cli layout)?

```sh
chekov pull unsloth/MiniMax-M2.7-GGUF:UD-Q5_K_XL --model-loc /Volumes/jane/models
```

Files found under `<loc>/<RepoName>/<rfilename>` (or `<loc>/<rfilename>`)
are **size-verified against the Hugging Face API and hard-linked** into
`<loc>/<name>@<rev12>/` — instant, zero extra space, and a truncated shard
can never be adopted silently (a mismatch warns and downloads instead).
The registry stores the absolute path; everything else works unchanged.

## Command reference

| Command | What it does |
|---|---|
| `run [name] [--daemon]` | Start llama-server (default: active model). Refuses loudly if: shard missing, port occupied, wired limit below config, engine not built, or a server already running. |
| `stop` | SIGTERM via pidfile, 20 s grace, SIGKILL escalation with a warning. Detects and cleans stale pidfiles. |
| `restart [name]` | Stop (if running) then `run --daemon`; swaps models in one motion. |
| `status` | running/pid, model, revision, port, ctx, uptime, wired-limit actual (with system-default annotation) vs required, log tail path. |
| `pull <spec> [--name N] [--dry-run] [--model-loc DIR] [--license-url URL]` | Resolve revision, download (or adopt) quant-matching files, snapshot license + provenance, register. Idempotent: same spec+revision is a verified no-op; a NEW revision downloads but never repoints (that is `update`'s gated job). |
| `list` | Table: active marker, name, quant, size on disk, revision. |
| `use <name>` | Set the active model. Never auto-restarts — prints the restart hint. |
| `rm <name> [--yes]` | Remove a model and its files. Confirmation required; refuses the active or currently running model. |
| `show [name]` | Fully resolved server invocation + license provenance — zero mystery about what will run. |
| `doctor` | Five checks (below). Skipped is reported as SKIP, never PASS. |
| `setup [--dry-run]` | Engine clone/pull + cmake Metal build; creates `models/`/`logs/`; wired-limit verification (see Installation). Idempotent. |
| `update --engine\|--model\|--all [--dry-run]` | Engine: git pull + rebuild, reports old→new commit. Model: re-resolve the active repo; new revisions land in a new `@rev` dir, license is diffed and **any change stops for explicit confirmation (STOP-4)** before an atomic registry repoint. Old revisions are never auto-deleted. |
| `integrate hermes [--yes]` | Surgical merge into `~/.hermes/config.yaml` (details below). |
| `integrate claude` | Generate `bin/cclocal`; global Claude settings untouched. |
| `env` | Stdout-only `ANTHROPIC_*` exports; diagnostics to stderr; safe for `eval "$(chekov env)"`. |

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
chekov run --daemon           # start (refuses if something is off)
chekov doctor                 # full health pass — run after any change
chekov stop                   # clean shutdown
tail -f logs/llama-server.log # watch the server
```

Model load time: ~2 minutes for a ~158 GiB model from a fast external SSD.
`run --daemon` returns immediately; poll `curl -s localhost:8080/health`
(503 while loading, 200 when ready) or just run `chekov doctor`.

### Swapping models

```sh
chekov pull unsloth/GLM-5.1-GGUF:UD-IQ1_M --model-loc /Volumes/jane/models
chekov use glm-5.1            # or in one motion: chekov restart glm-5.1
chekov restart
chekov doctor
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
| `stale pidfile … cleaned` on `stop` | The server died earlier (check the log tail). Just `chekov run --daemon` again. |
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
         "--cache-type-k", "q8_0", "--cache-type-v", "q8_0"]

[models."minimax-m2.7"]
repo = "unsloth/MiniMax-M2.7-GGUF"
quant = "UD-Q5_K_XL"
revision = "d2a05ccf69491b03db0cc40b335aec14bdaf7198"
path = "/Volumes/jane/models/minimax-m2.7@d2a05ccf6949"   # absolute = --model-loc
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
wired_limit_mb = 180000   # required GPU wired memory before `run` proceeds
hermes_ctx_floor = 65536  # hard floor when a model is hermes_ok

[doctor]
canary_max_tokens = 1500
degenerate_run_len = 30
replacement_char_max_pct = 5
```

Unknown keys are rejected loudly (deny_unknown_fields), never ignored.
`CHEKOV_HOME` overrides the root directory (default `~/personal_dev/chekov`).

## Layout

```
config.toml          # machine tunables (above)
models.toml          # the registry — managed by pull/use/rm/update (gitignored)
models/<name>@<rev12>/     # weights + REVISION + LICENSE.snapshot/.provenance
                           # (or an absolute --model-loc dir)
logs/                # chekov.pid, chekov.model, llama-server.log
llama.cpp/           # engine checkout + Metal build (managed by setup)
bin/cclocal          # generated by `chekov integrate claude`
shell/chekov.zsh     # PATH + cclocal alias + completions (sourced from ~/.zshrc)
shell/_chekov        # generated zsh completions (make install)
```

## Development

```sh
make test   # cargo test — 80+ tests, all offline against fakes/fixtures
make lint   # cargo fmt --check && cargo clippy --all-targets -- -D warnings
```

- clippy runs pedantic+nursery under `-D warnings`; `clippy.toml` encodes the
  house limits (≤40-line functions, ≤3 args, bounded nesting)
- HTTP is a trait object (`HttpClient`) — tests inject canned responses; no
  test ever touches the network or a real llama.cpp
- TDD with committed-red history per module (`git log --oneline` shows the
  red→green pairs); repo-specific standards overrides live in `AGENTS.md`
- `deny.toml` is authored for CI supply-chain checks (`cargo deny`)
