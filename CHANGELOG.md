# Changelog

All notable changes to chekov are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[SemVer](https://semver.org/).

## [Unreleased]

### Added
- `launch`: local-directory marketplace plugins (`extraKnownMarketplaces`
  with `source = "directory"`) are mirrored into the session config dir so
  `enabledPlugins` resolves; `extraKnownMarketplaces` is now a carried key.

### Added
- `setup` and `update --engine` record the llama.cpp commit they built to
  `logs/chekov.engine`, and `chekov status` shows it — an unrecorded engine
  says so and names the command that records one, rather than being guessed.
- `update --engine` now says when the running server is still the previous
  engine, mirroring what `use` already does for models.

### Fixed
- README's `[defaults]` block omitted `-np 1`, so anyone following the docs to
  hand-edit `models.toml` silently reintroduced the shared-KV-slot context
  split. Two new tests parse README's own TOML fences and compare them against
  `Defaults::default()` and `FileConfig::default()`, so this class of drift now
  fails the build instead of shipping.
- README's KV-cache rule of thumb used the model's layer count and treated q8_0
  as one byte per element. Modern MoE and sliding-window models cache only a
  fraction of their blocks (a 4-5× overestimate), and q8_0 is 34 bytes per 32
  elements. Both corrected, with a pointer to the measured value in the server
  log.
- `docs/HOWTOS.md` claimed a 4-bit quant costs 0.5 GB per billion parameters,
  contradicting its own table on the next line (which implies ~0.6 GB/B).
  The table was right: llama.cpp promotes `output.weight` and the embeddings,
  so a "4-bit" Q4_K_M realises about 4.8 bits per weight. Also states that the
  table is weights-only, matching what `render_quant_table` already says.
- Confirmation prompts no longer report a phantom decline when there is no
  terminal. `confirm` read EOF from a non-tty stdin and reported the user as
  having declined, with a remediation ("re-run and answer 'y'") that cannot be
  followed in cron or launchd. It now checks for a tty first and fails with
  `ConfirmationRequiresTerminal`, which states the actual situation.
- proxy: a non-2xx from llama-server no longer collapses to `http status: 400`
  with the body discarded. ureq's `http_status_as_error` is turned off so the
  status is taken directly and the server's own `error.message` — the only
  thing that says *why*, e.g. a context overflow — reaches the user, bounded so
  a runaway body cannot become a 20 KB log line.
- proxy: inbound headers are bounded. The body had a 64 MB ceiling but the
  header loop had no count cap and no per-line cap, so a local peer could hold
  a proxy thread open indefinitely or grow a single line without limit. Now
  100 headers and 16 KB per line, and an over-long line reports the ceiling
  instead of the misleading "client closed the connection".
- `update --model`: the "old revision kept on disk" notice told the user to run
  `chekov rm <name>`, but `repoint` has already moved the registry entry to the
  NEW revision — following that advice deleted the weights just downloaded and
  orphaned the old directory permanently (nothing enumerates the models dir, so
  an unreferenced revision is invisible to `list`). The notice now names the
  stale directory by path and warns against `rm` explicitly.
- `restart` with no argument targets the active model, which may not be the one
  running. It now says so before stopping — resolving run-state before the stop
  clears it — and names `chekov restart <running>` for keeping the loaded model.
  Unloading and reloading 100+ GB is never silent.
- `integrate hermes`: a `model:` header carrying a trailing comment
  (`model:  # active`) was not recognised, so the merge prepended a SECOND
  top-level `model:` key and left the old block intact — ambiguous YAML in a
  live Hermes config. The block header is now matched on its key, not the
  whole line.
- `integrate hermes`: a `providers:` block indented with anything other than
  two spaces is now refused with a named remediation instead of silently
  gaining a mis-nested duplicate `chekov:` entry beside the stale one. The
  module is contractually forbidden from clobbering this file, so an
  un-editable shape is a loud refusal, not a guess.
- `launch`: `settings.json` and `.claude.json` carry the server API key and are
  now created 0600 inside a 0700 session dir (created with the mode, not
  chmod'd afterwards, so there is no world-readable window).
- The llama.cpp build no longer passes a bare `cmake -j`, which reached make
  with no job count — unbounded compile jobs with the jobserver disabled, on a
  machine that may be holding a 158 GiB model resident. Now one job per
  logical core.
- CI, release and the Makefile pass `--locked` on every cargo invocation that
  resolves dependencies, so a `Cargo.toml`/`Cargo.lock` drift fails the build
  instead of silently re-resolving inside the runner.
- `CARGO_REGISTRY_TOKEN` is scoped to the publish step instead of the whole
  `crates-io` job, where it was also live during `actions/checkout` and
  `dtolnay/rust-toolchain@master`.
- Documentation: `README.md` no longer claims "No async runtime" without
  qualification, and the `hf-hub` dependency comment no longer states that
  `blocking` avoids tokio — upstream defines that feature as `["tokio/rt"]`.
- `plugins`: a `plugin.json` `name` is no longer trusted as a path component.
  It was joined onto the marketplace cache dir and that path was then removed
  and recreated, so `"name": "../.."` reached outside the cache and an
  absolute name replaced it entirely. Unsafe names now fall back, loudly.
- `plugins`: `installed_plugins.json` and `known_marketplaces.json` are written
  atomically (process-unique temp + rename, mirroring `Registry::save`) rather
  than truncate-in-place — these are another tool's live state files.
- release: `shell/chekov.zsh` ships under `shell/` in the tarball instead of at
  the root. It derives `CHEKOV_HOME` from its own depth, so the flattened
  layout resolved one level too high and every tarball install had a wrong
  `CHEKOV_HOME`, a wrong `PATH`, and unreachable completions.
- `launch` now runs the same four refusal gates as `run` before starting a
  server, instead of spawning blind and surfacing an opaque connection error.
- `launch` refuses to adopt a running server that is serving a different model
  (`ServerModelMismatch`) or one whose identity is unrecorded
  (`ServerModelUnknown`) — it no longer advertises a model and context window
  the upstream is not actually serving.
- The proxy translates an upstream `error` frame into an Anthropic `error`
  event and suppresses the terminal `end_turn`, so a failed turn is no longer
  reported to the agent as a clean, complete one.
- A mid-stream upstream failure now emits a terminating envelope instead of
  dropping the socket, which the SDK reported as a protocol error.

### Changed
- `[limits] wired_limit_mb` default is 187000 (was 200000). The old value
  exceeded macOS's own 75%-of-RAM default on every shipping Apple Silicon Mac,
  so a fresh install refused to `run` until the user hand-wrote a config file.
  README and `config.example.toml` now agree with the code.
- A wired-limit requirement above physical RAM reports `WiredLimitUnreachable`,
  naming `config.toml`, instead of printing a `sudo sysctl` command the machine
  can never satisfy.
- Default root is `~/.chekov` (was `~/personal_dev/chekov`); `CHEKOV_HOME`
  still overrides, and `shell/chekov.zsh` exports it for source installs.
- README: "Finding a model" / "Adding a model" guides; machine-specific
  paths removed.

## [0.1.0] — 2026-08-21

First release.

### Added
- Model lifecycle: `pull`, `list`, `use`, `rm`, `show` against a TOML registry
  with revision-pinned Hugging Face downloads, size-verified adoption of
  existing weights (`--model-loc`), and license snapshot + provenance per pull.
- Server lifecycle: `run` (background by default), `stop`, `restart`, `status`
  with pidfile handling and wired-memory verification before start.
- `doctor`: five offline-verifiable health checks (OpenAI door, Anthropic door,
  think-tag retention, NaN canary, context floor).
- `setup` / `update --engine|--model|--all`: llama.cpp clone + Metal build,
  gated model re-pointing with a license-diff stop.
- Integrations: `integrate hermes` (surgical config merge), `integrate claude`
  (`cclocal` launcher), `env`, and `launch claude` with an in-process
  Anthropic→OpenAI protocol translator and a generated `CLAUDE_CONFIG_DIR`.
- zsh completions and a `chekov.zsh` shell shim.
