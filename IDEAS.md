# IDEAS — chekov

New capability ideas live here, not in code (charter N13). An idea is a one-line
proposal with a one-line rationale; it becomes work only after the human
approves it and it is moved into a phase/task. Nothing here is implemented until
it is approved.

<!-- Add new ideas below. Format:
## <short title>
<what + why, one or two lines>
Proposed <date> — status: OPEN / APPROVED / DEFERRED
-->

## Model-fit sizing (2026-08-21)
A reference for deciding whether a GGUF fits before registering it in
`models.toml`. Mirrors chekov's `verdict_for` math (`TIGHT_FRACTION_PCT = 85`,
config MB treated as MiB, weights-only vs RSS+KV caveat) and the machine's
182.62 GiB wired budget. See the `chekov-development` skill
`references/model-fit-sizing.md`.
Proposed 2026-08-21 — status: documented.

## CLI evolution recipes (2026-08-21)
Worked recipes for flipping a flag's default while keeping a hidden back-compat
alias, and folding an overlapping subcommand into another as a mode flag. See
the `chekov-development` skill `references/cli-evolution.md`.
Proposed 2026-08-21 — status: documented.
