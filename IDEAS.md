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

## Pin the llama.cpp engine to a ref (2026-08-25)
`setup` clones llama.cpp master with no ref and `update --engine` fast-forwards
it, so the engine chekov builds is whatever upstream HEAD was that day. Model
weights are revision-pinned; the binary that runs them is not. Proposal: an
`[engine] git_ref` config key, `git fetch origin <ref>` + `checkout --detach
FETCH_HEAD` instead of `pull --ff-only`, and `--branch <ref>` on the clone.
Deferred from the provenance work, which only records the built commit — pinning
adds config surface and changes what `setup` does on every machine.
Proposed 2026-08-25 — status: OPEN

## Verify the engine binary after building it (2026-08-25)
`update --engine` never runs the binary it just built: a llama.cpp change that
breaks the build's output surfaces later, as a failed `run`. Proposal: a fourth
`EngineStep` running `llama-server --version`, so a bad build fails as
`EngineStepFailed` naming the step, and `--dry-run` prints it like the others.
Auto-rollback was considered and rejected — `git checkout <before>` plus a full
rebuild is a multi-minute silent side effect, the opposite of the loud-failure
creed. With the commit now recorded in `logs/chekov.engine`, a manual revert has
something to name.
Proposed 2026-08-25 — status: OPEN

## Replace hf-hub with the ureq already in the tree (2026-08-25)
`hf_hub` appears at exactly one call site (`core/hub.rs:363`) and its three-call
surface is `new()` / `model()` / one `download_file()` builder, yet it pulls 225
of the crate's 256 transitive dependencies — including tokio, reqwest, hyper and
the xet stack. chekov already queries the HF API over ureq at `hub.rs:126`.
Against it: hf-hub provides resume and Xet-accelerated transfer, which matter for
100+ GB pulls; hand-rolling means Range-request resume and losing Xet.
Proposed 2026-08-25 — status: OPEN

## `chekov stop --if-running` (2026-08-26)
A teardown script cannot call `stop` idempotently: stopping an already-stopped
server exits 1, and every error class shares exit 1, so the script cannot tell
that benign case from a real failure. Proposal: an opt-in `--if-running` flag
that prints "nothing to stop" and exits 0. Opt-in, not the default — a silent
no-op by default would weaken the loud-failure creed. A new flag is new
capability, so it waits here.
Proposed 2026-08-26 — status: OPEN

## `update --accept-license-change` for unattended runs (2026-08-26)
`update --model` cannot run in cron once a vendor changes their license text:
the STOP-4 gate needs a tty. The confirmation now says so plainly rather than
reporting a phantom decline, which is the honest half of the fix. Whether to
ALLOW unattended acceptance is a separate policy call — update.rs:147 says
"STOP-4: explicit confirmation, never assumed", and no evidence in the repo
shows anyone running `update --model` unattended. Only add the flag if that
changes.
Proposed 2026-08-26 — status: OPEN

## A live context-window check in `doctor` (2026-08-27)
`doctor`'s fifth row compares `models.toml` to `config.toml` and nothing else,
so it is the one row that can report PASS while the server is down. It is now
named "context floor (config, not the server)" so it cannot be misread as
evidence of health — but nothing yet verifies the context the SERVER actually
loaded, which can differ from the registry's intent indefinitely (the
`status-reports-registry-not-server` finding). Proposal: a sixth row probing
llama-server's `/props` for `n_ctx` and comparing it to the effective
`ctx_size`. A new check is new capability, and it touches every "five checks"
doc surface, so it waits here.
Proposed 2026-08-27 — status: OPEN
