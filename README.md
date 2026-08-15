# chekov

Local llama.cpp inference stack manager for Apple Silicon (Mac Studio M3
Ultra). One binary owns the full lifecycle — **pull → run → stop/restart →
doctor → update** — with models abstracted behind a registry so adding one is
a single `pull`, ollama-style.

Package name is `chekov-mac` (crates.io `chekov` is taken); the binary is
`chekov`.

## Quickstart

```sh
make setup        # build chekov (release) + clone/cmake llama.cpp with Metal
# if setup reports the wired limit low, run the sudo command it prints, then:
make setup        # re-run to verify — chekov never executes sudo itself

chekov pull unsloth/MiniMax-M2.7-GGUF:UD-Q5_K_XL
chekov use minimax-m2.7
chekov run --daemon
chekov doctor     # five health checks; non-zero exit on any failure

make install      # cargo install + zsh integration (chekov, cclocal, completions)
```

## Command reference

| Command | What it does |
|---|---|
| `run [name] [--daemon]` | Start llama-server (default: active model). Refuses loudly if the shard is missing, the port is occupied, the wired limit is low, or a server is already running. |
| `stop` | SIGTERM via pidfile, 20 s grace, SIGKILL escalation with a warning. Cleans stale pidfiles. |
| `restart [name]` | Stop (if running) then `run --daemon`; swaps models in one motion. |
| `status` | running / pid / model / revision / port / ctx / uptime / wired-limit actual-vs-required / log tail path. |
| `pull <spec>` | Resolve revision, download quant-matching files, snapshot license, register. `--dry-run` plans without downloading; `--name` overrides the derived slug; `--license-url` also snapshots a base-model license. |
| `list` | Table: name, quant, size on disk, revision, active marker. |
| `use <name>` | Set the active model. Never auto-restarts — prints the restart hint. |
| `rm <name>` | Remove a model (confirmation required, `--yes` to skip). Refuses the active or running model. |
| `show [name]` | The fully resolved server invocation + license provenance. |
| `doctor` | Five checks: OpenAI door, Anthropic door, think-tag retention, NaN canary, hermes ctx floor. Skipped ≠ passed. |
| `setup [--dry-run]` | Clone/pull + cmake Metal build of llama.cpp; verifies `iogpu.wired_limit_mb` and prints (never runs) the sudo command. Idempotent. |
| `update --engine\|--model\|--all [--dry-run]` | Engine: git pull + rebuild, old→new commit. Model: re-resolve, download new revision to a new dir, **stop on any license diff**, then atomically repoint. Old revisions are never auto-deleted. |
| `integrate hermes [--yes]` | Write `~/.hermes/config.yaml` (backup first; confirmation when replacing a non-custom provider; hard error if the active model's ctx is below 65536 while `hermes_ok`). |
| `integrate claude` | Generate `bin/cclocal` — Claude Code against the local server. Global Claude settings are never touched. |
| `env` | Stdout-only `ANTHROPIC_*` exports; diagnostics on stderr. Safe for `eval "$(chekov env)"`. |

## Pull-spec grammar

```
chekov pull org/repo                 # no quant → error listing available tags (no silent default)
chekov pull org/repo:QUANT           # e.g. unsloth/MiniMax-M2.7-GGUF:UD-Q5_K_XL
chekov pull org/repo:QUANT@rev       # explicit revision pin
chekov pull org/repo@rev
chekov pull https://huggingface.co/org/repo        # normalized to org/repo
```

The short name is derived from the repo tail: `-GGUF` stripped, lowercased
(`unsloth/MiniMax-M2.7-GGUF` → `minimax-m2.7`). Override with `--name`.

## Model swap

```sh
chekov pull unsloth/DeepSeek-V4-Flash-GGUF:UD-Q4_K_XL
chekov use deepseek-v4-flash       # or skip use and: chekov restart deepseek-v4-flash
chekov restart
chekov doctor
```

No file edits required — the registry (`models.toml`) is the single source of
truth, and `chekov show` prints exactly what will run.

## Flags inheritance: concatenate, not replace

`models.toml` has a `[defaults]` table (ctx_size, flags) and per-model tables.
A model's effective flags are **`defaults.flags` followed by its
`extra_flags`** — extras append, they never replace the defaults. `ctx_size`
is the one scalar override: a model's value wins over the default when set.
Verify any model's resolution with `chekov show <name>`.

Note: `active` is a top-level key and must appear **above** the `[defaults]`
table in the file (TOML top-level keys can't follow a table header).

## License snapshots — why

Each pull writes `LICENSE.snapshot` and `LICENSE.provenance` (repo, revision,
source URL, UTC) next to the weights. Model vendors have changed license text
after release; because `update --model` diffs the new revision's license
against the snapshot and **stops on any change** (STOP-4), you can never be
silently migrated onto different terms. The snapshot is the contemporaneous
record of what you agreed to when you downloaded the weights.

## Layout

```
config.toml        # optional overrides: [server] host/port/api_key,
                   # [limits] wired_limit_mb/hermes_ctx_floor, [doctor] thresholds
models.toml        # the registry (managed by pull/use/rm/update)
models/<name>@<rev12>/   # weights + REVISION + LICENSE.snapshot + LICENSE.provenance
logs/              # chekov.pid, chekov.model, llama-server.log
llama.cpp/         # engine checkout + build (managed by setup/update --engine)
bin/cclocal        # generated by `chekov integrate claude`
shell/chekov.zsh   # PATH + alias + completions (sourced from ~/.zshrc)
```

`CHEKOV_HOME` overrides the root (default `~/personal_dev/chekov`).

## Development

```sh
make test   # cargo test — all tests run offline against fakes and fixtures
make lint   # cargo fmt --check && cargo clippy --all-targets -- -D warnings
```

Standards: see `AGENTS.md` for this repo's §12 overrides on top of the
machine-wide coding standards (annex §C Rust).
