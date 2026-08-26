# Changelog

All notable changes to chekov are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[SemVer](https://semver.org/).

## [Unreleased]

### Added
- `launch`: local-directory marketplace plugins (`extraKnownMarketplaces`
  with `source = "directory"`) are mirrored into the session config dir so
  `enabledPlugins` resolves; `extraKnownMarketplaces` is now a carried key.

### Fixed
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
