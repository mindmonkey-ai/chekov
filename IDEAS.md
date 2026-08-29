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
SHIPPED 2026-08-29 as `[engine] git_ref` (branch, tag, or commit). Absent means
today's behaviour exactly — nothing changes until a machine opts in. Pinned:
`git fetch origin <ref>` + `checkout --detach FETCH_HEAD` (never a pull; no
`--branch` on the clone, so a sha is a valid pin); `update --engine` prints
`(pinned to <ref>)`. A ref starting with `-` or containing whitespace is
refused at config load naming the key — git would read the first as an option.
Why it earned its place: on 2026-08-28 the engine had to be moved to a
master-plus-one-cherry-pick branch by hand to gain `qwen4exp`, and `update
--engine` on the old fix branch reported `dda1b0d67 → dda1b0d67`.
Proposed 2026-08-25 — status: SHIPPED

## Verify the engine binary after building it (2026-08-25)
`update --engine` never runs the binary it just built: a llama.cpp change that
breaks the build's output surfaces later, as a failed `run`. Proposal: a fourth
`EngineStep` running `llama-server --version`, so a bad build fails as
`EngineStepFailed` naming the step, and `--dry-run` prints it like the others.
Auto-rollback was considered and rejected — `git checkout <before>` plus a full
rebuild is a multi-minute silent side effect, the opposite of the loud-failure
creed. With the commit now recorded in `logs/chekov.engine`, a manual revert has
something to name.
SHIPPED 2026-08-29: `setup_steps` ends with `<engine>/build/bin/llama-server
--version` as its own `EngineStep` ("verify the built llama-server runs"), on
both the pinned and the unpinned path; it prints under `--dry-run` and fails
as `EngineStepFailed` naming the step. No rollback, as decided above.
Proposed 2026-08-25 — status: SHIPPED

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
SHIPPED 2026-08-29 exactly as proposed: the flag covers only "no pidfile at
all"; a stale pidfile is still cleaned and reported (already exit 0), and a
stop that fails still fails, flag or not.
Proposed 2026-08-26 — status: SHIPPED

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
SHIPPED 2026-08-29 as the sixth row, "context loaded (server /props)",
reusing the bench's `runner::assert_props_ctx` verbatim (doctor and bench
cannot disagree). `/props` is behind `--api-key`, so doctor passes
`serve::get_bearer` through the bench's `PropsFetch` seam — no change to the
`HttpClient` trait or any fake. Unreachable = FAIL, like the other server
rows. Every "five checks" surface now says six.
Proposed 2026-08-27 — status: SHIPPED

## Machine capability scan, frontier graph, recommendations and agent bench (2026-08-25)
`chekov capability {scan,graph,recommend,explain,bench,compare}` — probe the machine
(sysctl / ioreg / `llama-server --list-devices` / df+mount), render an ASCII+SVG frontier of
model x quant x ctx with fits/tight/exceeds and predicted-vs-measured tok/s, recommend
candidates with the sizing math shown, and benchmark them through chekov's own
Anthropic->OpenAI translator against a built-in fixture or the user's own repo.
Motivated by a measured defect: `checks::effective_wired_mb` reports 196608 MiB on this
machine where the engine reports 228065 MiB — chekov understates its own budget by 30.7 GiB.
Verified 2026-08-27: `./llama.cpp/build/bin/llama-server --list-devices` prints
`MTL0: Apple M3 Ultra (228065 MiB, 228064 MiB free)`.
Supersedes the arithmetic in `references/model-fit-sizing.md` (see "Model-fit sizing", above).
Proposed 2026-08-25 — status: **slices 1-3 SHIPPED; slice 4 SHIPPED without the compiled-in seed catalog (human's call 2026-08-27: a vendored list rots; --refresh is the discovery layer); slice 5 harness SHIPPED 2026-08-27, upgraded 2026-08-28 with the §7.4-§7.5 stamp + JSONL store (17-field stamp, first-differing-field compare refusal, --resume, pinned sampling); slice-5 gap part 2 (per-candidate lifecycle §7.3: --models, flag hygiene, Metal env, teardown+release check, confirm/dry-run, cache_n) SHIPPED 2026-08-28; part 3 (probe suites §7.2) v0 SHIPPED 2026-08-28 (--suite agentic: tool_emit/grammar_gap/instruction seed set, growing toward 30/40; deferred: diff_fidelity+tool_loop+long_ctx_trace+hallucination need the §8/§9 corpora, think_leak waits on §13 Q5); slice-5 "`--metric tok-s` upgrades from predicted to measured" SHIPPED 2026-08-28 (fixed bands, deepest-depth median, exact-match + stale footer); fixture-v1 content release-gated; slice 6 OPEN (`--svg` SHIPPED 2026-08-28; `--codebase`, `--judge`, throughput dots OPEN)**

## A forcing mechanism for `grammar_gap` on thinking-prefill templates (2026-08-28)
`response_format` json_schema is refused (HTTP 400, "Failed to initialize
samplers") by this engine for `ornith-1.5-35b-a3b`, so the §7.2 grammar_gap
axis reports N/A on it. Root cause, verified in source and reproduced
live: on `/v1/chat/completions` llama.cpp builds a grammar whose root is
`"<|im_start|>assistant\n" space response-format`, then prefills the FULL
generation prompt — which this template ends with `<|im_start|>assistant\n<think>\n`
— through that grammar sampler. The root cannot accept the `<think>\n` the
template itself emitted, so sampler init throws before a single token is
generated. `/completion` (no generation prompt, no prefill) accepts every
schema shape including oneOf+const, so the schema converter is not at fault.

Candidate mechanisms, and where each stands:

(a) **raw GBNF via the `grammar` field — REJECTED, do not build this.** It does
    return 200 and did produce a correct forced call for te-002 (raw grammars
    are USER-type and skip the prefill), which makes it look attractive. It is
    strictly worse than the current N/A. The reply comes back as
    `<think>\n{...}`: the template's `<think>` is prompt-emitted, so no grammar
    rule can consume it, and because the grammar forbids `</think>` the span
    never closes. `strip_thinking` refuses an unterminated span BY DESIGN, so
    grading sees no text and every forced case becomes a SILENT failure —
    trading loud engine errors for quiet fabricated model failures, the exact
    trade this axis exists to prevent. Making it correct would mean hardcoding
    each model family's reasoning-tag convention into the grammar root, and
    would forbid reasoning in the forced arm while the unconstrained arm
    reasons freely — a confound injected into the very number designed to
    detect self-deception.

(b) **per-request `"reasoning_format":"deepseek"` — VALIDATED 2026-08-29, SHIPPED.**
    Returns 200 on the same schema that 400s, because that flag gates whether
    the `<think>` alternative enters the grammar (`chat.cpp:1187`
    `extract_reasoning`). Live on engine 0f194b907 with `max_tokens=200`:
    `content` = `{"name": "get_weather", "arguments": {"location": "Paris"}}`,
    the reasoning in `reasoning_content`, `finish_reason: stop`. The earlier
    doubt was only the 30-token budget. Built as `runner::FORCED_REASONING_FORMAT`
    on the forced wire ONLY (the unconstrained and streamed wires are
    byte-identical to before); the run head records it and the `grammar_gap`
    line prints `forced pass ran with reasoning extracted (deepseek)`, so the
    one extra difference from the unconstrained arm is named, not hidden.
    Also established 2026-08-29: the human's cherry-picked fix (0f194b907,
    `chat-auto-parser-generator.cpp`) is correct for AUTOPARSER templates
    (MiniMax-M2) but ornith's template (`<tool_call>` + `<think>` +
    `<|im_start|>`) is routed to the SPECIALIZED handler at `chat.cpp:1166-1300`
    (hardcoded `GEN_PREFIX`), whose grammar root the server logged as
    `root ::= "<|im_start|>assistant\n" space response-format` — no `<think>`
    alternative. The upstream re-port belongs at `chat.cpp:1233-1239`; chekov
    does not wait for it.

(c) **Patch llama.cpp upstream — worth a PR, but sequence nothing behind it.**
    The narrow fix is in `chat.cpp`'s specialized handlers: build the prefix
    from `data.generation_prompt` rather than the hardcoded `GEN_PREFIX`, or
    admit the `<think>` alternative whenever `supports_reasoning` regardless of
    `extract_reasoning`. chekov must never depend on it: chekov tracks
    tip-of-master with no pin, users run whatever they built, and an upstream
    merge does not retroactively repair anyone's binary.

Open question for the human: §7.5 says an N/A axis withholds the composite,
but §7.5's weight table gives `grammar_gap` ZERO weight — it is a diagnostic
control, not a scored axis. Withholding on it would make the composite
permanently unobtainable on any llama.cpp build with a thinking template.
Decide before a composite is implemented.

Note a false-pass hazard for whoever builds this: an EMPTY schema, and
`response_format: {"type":"json_object"}`, both return 200 with UNCONSTRAINED
prose — no grammar is attached at all. A preflight probing with `{}` would
conclude structured output works and then fabricate passes, the mirror image
of the failures this N/A change removed. Probe with a non-empty schema only.
Proposed 2026-08-28 — status: OPEN

## Streaming probes for bench (2026-08-28)
Spec §7.1 asked for probes over the STREAMING seam as well ("what makes
streaming-only defects reachable — interleaved parallel tool-call deltas, an
upstream error frame swallowed into a fake `end_turn`, an unterminated
`<think>` eating the turn"). Only the non-streaming half was built, and the
first agentic run found exactly that class of bug: the streaming translator
stripped thinking spans while the non-streaming one did not, so the two halves
of the same translator disagreed about what the agent receives. Claude Code
streams; the bench did not — so the bench was grading a path the agent never
takes. Fixed for thinking, but the asymmetry class remains until probes cross
`stream_translator()` the way `serve::relay` does.
SHIPPED 2026-08-28: `runner::cross_streaming` puts `stream: true` on the
Anthropic request, pumps the SSE body through a fresh `stream_translator()`
exactly as `serve::relay` does, and reassembles the agent-side events into the
message an SDK client holds at `message_stop`, so the same graders read it.
Every unconstrained agentic case now crosses BOTH doors (no flag — Claude
Code's door is not optional); rows carry `transport`; the report prints
`asymmetry <suite> <case>: buffered PASS, streamed FAIL — <reason>` for every
case that disagrees with itself. An `error` frame is `BenchStreamFailed` —
recorded unavailable, never a forged `end_turn`. Still buffered by design: the
throughput sweep (its numbers are upstream timings either way) and the
grammar-forced pass (its axis is the grammar gap). Still out of reach, as §7.1
states: the socket — `serve.rs`'s HTTP/1.1 framing and chunked encoding.
Proposed 2026-08-28 — status: SHIPPED

## `EndpointDown` claims "not answering" for a request that WAS answered (2026-08-28)
A 400 refusal renders as "endpoint ... is not answering ... restart with
`chekov restart`". The endpoint answered — it refused — and restarting cannot
help when the request itself is unacceptable. Surfaced by the grammar_gap N/A
message, whose remediation advice is actively misleading. Wants a distinct
variant for "the upstream refused this request" carrying the server's own
explanation, now that `hub::post_json` preserves it.
SHIPPED 2026-08-29 as `ChekovError::UpstreamRefused { url, status, reason }`
via one classifier, `serve::answered`, used by `hub::post_json` and
`get_bearer`: 2xx is the body, anything else is a refusal carrying the
status and the server's own words, and the message says the server is up and
the request is what to fix (`chekov show`, logs/llama-server.log).
`EndpointDown` keeps its meaning — connect/send/read failures, readiness
timeouts, an unparseable `/props`. The bench's forced-pass latch now fires
on `UpstreamRefused` only; before, a dead socket mid-run would have been
written off as an engine limitation.
Proposed 2026-08-28 — status: SHIPPED

## The non-streaming translator drops `reasoning_content` (2026-08-28)
`to_anthropic_response` reads `message.content` and `message.tool_calls` only.
A model served with `--reasoning-format auto|deepseek` puts its reasoning in
`message.reasoning_content`, which the streaming path turns into a `thinking`
block and the non-streaming path silently discards (§C.2: nothing degrades
silently). Not currently reachable — every registry entry uses
`--reasoning-format none` — which is why it is filed rather than fixed.
Became reachable 2026-08-29 when the bench's forced pass started asking for
`reasoning_format: deepseek` per request. FIXED the same day: a non-empty
`reasoning_content` is the first content block, `{type: thinking, thinking,
signature: ""}` — the block `ClaudeStream::on_thinking` opens — ahead of the
text and `tool_use` blocks; a test holds the two paths to the same block
sequence. Graders read text blocks only, so bench verdicts are unchanged; the
stored artifact simply stops losing the reasoning.
Proposed 2026-08-28 — status: FIXED

## Bench GLM-5.3-Flash — blocked on upstream llama.cpp (2026-08-28)
`unsloth/GLM-5.3-Flash-GGUF` (arch `glm5_next`, released 2026-08-26) needs
llama.cpp PR #27754 (https://github.com/ggml-org/llama.cpp/pull/27754), which
is not on `master` as of 2026-08-28 (`d7bd3bfca`): `llama-arch.cpp` there has
no `glm5*` entry. `chekov update --engine` tracks master only, so the engine
cannot reach it, and building from a PR branch would stamp every run with a
non-master commit that no later run could compare against. Human's call
2026-08-28: skip until the PR merges. When it does: `chekov update --engine`,
then `chekov pull unsloth/GLM-5.3-Flash-GGUF:UD-Q3_K_XL --model-loc
/Volumes/jane/models` (137.4 GiB; UD-Q4_K_XL is 186 GiB and tight against the
222.7 GiB budget before KV), then `chekov capability bench --models
glm-5.3-flash`. Qwen3.8-Flash-Next (`qwen4exp`) IS on master and is being
benched in the same pass.
Proposed 2026-08-28 — status: BLOCKED (upstream)

## A cell's second character ignores the overhead's provenance (2026-08-28)
`frontier::Cell::inputs()` reports `#` (measured) whenever KV is measured,
regardless of `overhead_bytes.provenance` — and `build_frontier` gives every
cell a flat predicted 3 GiB overhead. So a cell can print "measured" while one
of its three summands is a constant guess. Defensible as shipped (KV is the
term that varies with context and dominates the total; a second character that
was always `·` would carry no information), but it is a real gap between the
glyph and the arithmetic. The SVG's per-cell tooltip prints each part's own
provenance, which is the honest version; the glyph is the lossy summary.
Noticed while building `--svg`; not changed there, because it would alter
shipped terminal output and its tests.
Proposed 2026-08-28 — status: OPEN

## Throughput dots in the SVG (2026-08-28)
Spec §5 wants the SVG to carry measured throughput as filled dots with p10-p90
whiskers and predicted throughput as hollow dots with a ±15% range. Not built:
`Frontier` carries no speed at all, because the "`--metric tok-s` grid upgrades
from predicted to measured" line is still deferred (see below). Blocked on the
same work — once stored bench medians reach the frontier model, both the ASCII
grid and the SVG gain the layer together, from one source.
UNBLOCKED 2026-08-28: `Cell.speed` now carries the measured median with p10-p90,
so the filled-dot layer has its source. The hollow predicted dots do not — no
predicted tok/s reaches the frontier model, and the ±15% band is an unvalidated
prior — so the layer should ship measured-only first.
Proposed 2026-08-28 — status: OPEN

## Feed measured bench medians into `capability graph` (2026-08-27)
Slice 5's spec line "the `--metric tok-s` grid upgrades from predicted to
measured" is deliberately deferred from the harness change: wiring stored
`logs/bench/` medians into the slice-2 grid touches every graph rendering
surface and needs a staleness rule (a measurement from an older
engine.build_commit must not silently pose as current). Do it as its own
change once a few real runs exist.
SHIPPED 2026-08-28 as `capability graph --metric tok-s`. Two decisions
recorded here because they deviate from or sharpen the spec: (1) the band
digit uses FIXED edges (5/10/15/20/30/40/60/80 tok/s), not the §5.2 "deciles
of decode rate" — deciles of the peer set move a cell's digit when a
different model is benched, the objection §7.5 already adopted for
composites; (2) the headline per run is the decode median at the DEEPEST
summarisable depth, named in the legend — the closest to an agent loop with a
full context, where a shallow probe flatters every model. A run applies to a
cell only on an exact model+quant+ctx+machine match; the latest of several is
shown and the choice is a footnote; rule 8's stale footer names both builds.
Proposed 2026-08-27 — status: SHIPPED

## Tool-parser gate: report, do not refuse (2026-08-27)
Slice 4 of the capability spec makes "falls through to llama.cpp's generic PEG
autoparser" a HARD REFUSAL under `--role agent`. Replaying the real cascade
(`llama.cpp/common/chat.cpp` ~3430-3552) against live templates shows that gate
would reject `unsloth/MiniMax-M2.7-GGUF` — the author's own daily driver, marked
`hermes_ok = true` in `models.toml`. Its 6594-char template carries
`<minimax:tool_call>` and `<invoke name=` but not the `]<]minimax[>[` namespace
token that llama.cpp's only MiniMax arm (M3) requires, so it falls through and
still works. Fallthrough means "no dedicated parser", not "cannot call tools".
`core::toolparser` therefore classifies and reports; it does not refuse.
RESOLVED 2026-08-27 by the human: `recommend --role agent` DOWNRANKS a
fallthrough candidate with a printed note rather than rejecting it. Implemented.
Proposed 2026-08-27 — status: RESOLVED
