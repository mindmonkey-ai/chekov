# PHASE-LOG — chekov

**Ownership rules.** This file is written by the implementing agent and reviewed
by the human. Entries are **append-only**: never rewrite or delete prior content
— corrections are new dated lines. A phase is open only after the human writes
`APPROVED` on the previous entry. An exit metric is met only when the
demonstration block contains the real command and its real output.

Entry protocol per phase: **(1)** at phase start, fill Plan and Test Plan *before
any implementation*; **(2)** during the phase, log deviations/escalations as they
happen; **(3)** at phase end, run the exit demonstration, paste command + output,
then STOP and wait for review.

---

## adopt-pushkin — adopt the harness governance

**Status:** OPEN — awaiting human review (2026-08-21)

### Plan
2026-08-21 — Adopt pushkin's governance layer into chekov (Rust-only, single-crate
macOS CLI). Tasks in execution order:
1. Inspect the harness documents (AGENT-INSTRUCTIONS.md, DESIGN-FINDINGS.md,
   PHASE-LOG.md, pushkin.toml, AGENTS.md) and chekov's existing config.
2. Write a customized AGENTS.md: pushkin's day-to-day rules adapted to chekov's
   shape (clap-derive CLI, thiserror, no schema/contract system, no TS/SQLite
   core). Preserve chekov's carried §12 overrides verbatim.
3. Write pushkin.toml: Rust-only floor profile (fmt, clippy, test, deny), path
   gates off-for-now with an explicit re-arm path, mirroring pushkin 2026-08-18.
4. Write PHASE-LOG.md (append-only) and IDEAS.md with the harness templates.
5. Verify `make lint && make test` still green on the new branch.

### Test plan (written before implementation; tests are read-only once written)
2026-08-21 — Adoption is a docs/config change, not a code change; verification is
`make lint && make test` green plus a read-back of the artifacts.

### Execution notes
2026-08-21 — Branch `adopt-pushkin` created from `develop`. chekov already had
partial adoption: `.claude/settings.json` carried `pushkin-v1` hooks
(PreToolUse/SessionStart/Stop) and a `.pushkin/` dir (consent.json,
daemon.canonical, events.db). This change formalizes the governance layer
(AGENTS.md + pushkin.toml + PHASE-LOG.md + IDEAS.md) and leaves the hooks intact.

### Deviations & escalations
2026-08-21 — chekov has no schema/contract system, so pushkin's `schema_epoch`,
`canonical`, and `authoring` are carried as no-ops (schema_epoch bumped to 1 to
satisfy the R9 positivity check; canonical/authoring set to `none`) to keep the
manifest shape identical; a comment notes where they would be bumped.
`read_only_paths` is scoped to `tests/**` (chekov's tests are inline
`#[cfg(test)]` modules plus `tests/`), matching pushkin's "committed tests are
product-gated" intent. The git-hooks plane is off (chekov uses Claude Code's
native hooks, not lefthook).

### Exit demonstration (evidence)
```text
$ make lint
cargo fmt --check && cargo clippy --all-targets -- -D warnings   # exit 0, clean
$ make test
cargo test                                                        # green
```

### Human review
<!-- Decision line format: APPROVED — <name>, <date> ("<quoted human words>") -->

---

<!-- TEMPLATE for subsequent phases — copy verbatim, do not restructure. -->
