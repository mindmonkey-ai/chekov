# Contributing to chekov

## Setup

```sh
git clone https://github.com/mindmonkey-ai/chekov.git && cd chekov
make setup && make install     # release build, llama.cpp Metal build, PATH + completions
make test && make lint         # the CI gate, locally
make deny                      # cargo deny — licenses, advisories, duplicates
```

Tests are fully offline: HTTP goes through the `HttpClient` trait and is
faked, and nothing spawns a real llama.cpp. Keep it that way.

`make test` never executes a benchmarked repository's build. The one test
that does — tiers 6-7 against a real `cargo` — is behind an env gate and
prints why it skipped:

    CHEKOV_TEST_EXEC=1 cargo test --locked --test codebase_exec -- --test-threads=1

## Ground rules

- **Branch from `main`, open a PR.** CI runs fmt, clippy (pedantic + nursery,
  `-D warnings`, with the `clippy.toml` limits: ≤40-line functions, ≤3
  arguments, bounded nesting), the test suite, and `cargo deny`.
- **No `#[allow(clippy::…)]` to get past a gate** — restructure instead. No
  `unwrap()`/`expect()` outside tests: release builds `panic = "abort"`.
- **Errors name their fix.** Every `ChekovError` message tells the user the
  command that remediates it. Nothing degrades silently.
- **Comments explain why, names explain what.** Don't delete a WHY comment
  you don't understand; ask.
- **Every serde struct read from disk or network** uses
  `deny_unknown_fields`, except files owned by another tool (e.g. Claude
  Code's plugin registry), which are read permissively and say so.
- Repo-specific standards overrides are recorded in `AGENTS.md`.

## Commits and releases

Conventional-commit prefixes (`feat`, `fix`, `docs`, `ci`, `chore`, `test`),
scope in parentheses. Add user-facing changes to `CHANGELOG.md` under
`[Unreleased]`. Releases are cut by tagging `vX.Y.Z` on `main` after bumping
`Cargo.toml` — see the README's "Cutting a release".
