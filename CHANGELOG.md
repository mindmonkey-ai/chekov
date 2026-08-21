# Changelog

All notable changes to chekov are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[SemVer](https://semver.org/).

## [Unreleased]

### Added
- `launch`: local-directory marketplace plugins (`extraKnownMarketplaces`
  with `source = "directory"`) are mirrored into the session config dir so
  `enabledPlugins` resolves; `extraKnownMarketplaces` is now a carried key.

### Changed
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
